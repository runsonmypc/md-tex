//! Single instance: the first launch listens on a per-user Unix socket; a
//! later launch sends its paths there and exits. Launches take turns under a lock file, so
//! two at once cannot both become the primary, and a socket file is removed only after a
//! connection to it was refused (left by a crashed instance).

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The socket in a per-user directory: `$XDG_RUNTIME_DIR` on Linux when set (and private,
/// as the spec has it), otherwise `md-tex-<uid>` in the temporary directory, created
/// private to this user. A shared `/tmp/md-tex.sock` would let another user take the name
/// and receive our paths, or have every launch hand its files to them and exit.
pub fn socket_path() -> std::io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty() && cfg!(target_os = "linux"))
        .map(PathBuf::from);
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let (dir, private) = socket_dir(runtime, std::env::temp_dir(), uid);
    if private {
        private_dir(&dir, uid)?;
    }
    Ok(dir.join(format!("{}.sock", crate::settings::APP_DIR)))
}

/// The directory for the socket, and whether it is ours to create and check. A runtime
/// directory that others can enter (some `su` and container setups point it at `/tmp`) is
/// passed over.
fn socket_dir(runtime: Option<PathBuf>, temp: PathBuf, uid: u32) -> (PathBuf, bool) {
    match runtime.filter(|r| check_private(r, uid).is_ok()) {
        Some(r) => (r, false),
        None => (
            temp.join(format!("{}-{uid}", crate::settings::APP_DIR)),
            true,
        ),
    }
}

/// Creates `dir` with mode 0700, or checks that the existing one is a real directory
/// owned by `uid` that no one else can enter.
fn private_dir(dir: &Path, uid: u32) -> std::io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    check_private(dir, uid)
}

/// Checks that `dir` is a real directory owned by `uid` that no one else can enter.
fn check_private(dir: &Path, uid: u32) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "{} is not a private directory of this user",
            dir.display()
        )));
    }
    Ok(())
}

pub enum Claim {
    /// This launch is the primary; serve the listener.
    Primary(UnixListener),
    /// The paths went to the running instance.
    Forwarded,
}

/// Becomes the primary, or forwards `paths` (made absolute) to the running one.
pub fn claim(socket: &Path, paths: &[PathBuf]) -> std::io::Result<Claim> {
    // Held until this launch has forwarded or is listening.
    let _lock = lock(&socket.with_extension("lock"))?;
    match UnixStream::connect(socket) {
        Ok(mut stream) => {
            stream.write_all(&encode(paths))?;
            Ok(Claim::Forwarded)
        }
        Err(e) => {
            match e.kind() {
                // Nobody listening: a socket file left by a crashed instance.
                std::io::ErrorKind::ConnectionRefused => std::fs::remove_file(socket)?,
                std::io::ErrorKind::NotFound => {}
                _ => return Err(e),
            }
            UnixListener::bind(socket).map(Claim::Primary)
        }
    }
}

/// An exclusive `flock` on `path`, released when the file closes.
fn lock(path: &Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    // SAFETY: the descriptor is open for as long as `file` lives.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

/// Paths are absolute (relative ones resolve against this launch's directory) and
/// separated by NUL, which no path contains.
fn encode(paths: &[PathBuf]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut out = Vec::new();
    for p in paths {
        out.extend_from_slice(cwd.join(p).as_os_str().as_bytes());
        out.push(0);
    }
    out
}

fn decode(bytes: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| PathBuf::from(std::ffi::OsStr::from_bytes(p)))
        .collect()
}

/// How long a forwarding launch may take to send its paths, in all: a deadline per read
/// let a client that sends a byte now and then hold up every later launch.
const READ_DEADLINE: Duration = Duration::from_secs(1);

/// The most a forwarding launch may send, which fits thousands of long paths. A longer
/// message is dropped rather than read into memory.
const MAX_MESSAGE: usize = 1 << 20;

/// Accepts forwarded launches on a thread, passing each one's paths (empty for a launch
/// without paths) to `on_open`.
pub fn serve(listener: UnixListener, on_open: impl Fn(Vec<PathBuf>) + Send + 'static) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            if let Some(bytes) = read_message(&mut stream) {
                on_open(decode(&bytes));
            }
        }
    });
}

/// The whole message, or `None` if it takes longer than `READ_DEADLINE`, is longer than
/// `MAX_MESSAGE` or fails.
fn read_message(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut bytes = Vec::new();
    let mut buf = [0; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        // macOS refuses the option (EINVAL) once the client has closed; the read then
        // ends at once anyway.
        let _ = stream.set_read_timeout(Some(left));
        match stream.read(&mut buf) {
            Ok(0) => return Some(bytes),
            Ok(n) if bytes.len() + n <= MAX_MESSAGE => bytes.extend_from_slice(&buf[..n]),
            Ok(_) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn second_launch_forwards_its_paths() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let Claim::Primary(listener) = claim(&socket, &[]).unwrap() else {
            panic!("first launch is the primary");
        };
        let (tx, rx) = mpsc::channel();
        serve(listener, move |paths| {
            let _ = tx.send(paths);
        });
        let paths = vec![PathBuf::from("/notes/a.md"), PathBuf::from("rel/b.md")];
        assert!(matches!(claim(&socket, &paths).unwrap(), Claim::Forwarded));
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            got,
            vec![PathBuf::from("/notes/a.md"), cwd.join("rel/b.md")]
        );
        assert!(matches!(claim(&socket, &[]).unwrap(), Claim::Forwarded));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn simultaneous_launches_make_one_primary() {
        for _ in 0..20 {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("app.sock");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let claims: Vec<_> = (0..8)
                .map(|_| {
                    let (socket, barrier) = (socket.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        claim(&socket, &[]).map(|c| match c {
                            // Keep the primary listening until every launch has claimed.
                            Claim::Primary(l) => Some(l),
                            Claim::Forwarded => None,
                        })
                    })
                })
                .collect();
            let claims: Vec<_> = claims.into_iter().map(|t| t.join().unwrap()).collect();
            let primaries = claims.iter().filter(|c| matches!(c, Ok(Some(_)))).count();
            assert_eq!(primaries, 1, "{claims:?}");
        }
    }

    #[test]
    fn a_client_that_never_finishes_does_not_block_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let Claim::Primary(listener) = claim(&socket, &[]).unwrap() else {
            panic!("first launch is the primary");
        };
        let (tx, rx) = mpsc::channel();
        serve(listener, move |paths| {
            let _ = tx.send(paths);
        });
        let mut stuck = UnixStream::connect(&socket).unwrap();
        stuck.write_all(b"/half").unwrap();
        let paths = vec![PathBuf::from("/notes/a.md")];
        assert!(matches!(claim(&socket, &paths).unwrap(), Claim::Forwarded));
        // The stuck client times out (its partial path is dropped); the next is served.
        assert_eq!(rx.recv_timeout(READ_DEADLINE * 3).unwrap(), paths);
        drop(stuck);
    }

    /// The timeout was per read: a client sending a byte every 700 ms held the server, and
    /// the next launch's paths waited for it.
    #[test]
    fn a_client_that_trickles_does_not_block_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let Claim::Primary(listener) = claim(&socket, &[]).unwrap() else {
            panic!("first launch is the primary");
        };
        let (tx, rx) = mpsc::channel();
        serve(listener, move |paths| {
            let _ = tx.send(paths);
        });
        let mut slow = UnixStream::connect(&socket).unwrap();
        let trickle = std::thread::spawn(move || {
            for _ in 0..50 {
                if slow.write_all(b"/").is_err() {
                    break;
                }
                std::thread::sleep(READ_DEADLINE / 4);
            }
        });
        let paths = vec![PathBuf::from("/notes/a.md")];
        assert!(matches!(claim(&socket, &paths).unwrap(), Claim::Forwarded));
        assert_eq!(rx.recv_timeout(READ_DEADLINE * 3).unwrap(), paths);
        trickle.join().unwrap();
    }

    /// A message of any length was read into memory.
    #[test]
    fn an_oversized_message_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let Claim::Primary(listener) = claim(&socket, &[]).unwrap() else {
            panic!("first launch is the primary");
        };
        let (tx, rx) = mpsc::channel();
        serve(listener, move |paths| {
            let _ = tx.send(paths);
        });
        let mut big = UnixStream::connect(&socket).unwrap();
        let sender = std::thread::spawn(move || {
            // The server stops reading, so this write may fail.
            let _ = big.write_all(&vec![b'a'; MAX_MESSAGE + 1]);
        });
        let paths = vec![PathBuf::from("/notes/a.md")];
        assert!(matches!(claim(&socket, &paths).unwrap(), Claim::Forwarded));
        let got = rx.recv_timeout(READ_DEADLINE * 3).unwrap();
        assert!(got == paths, "got a {}-byte path", got[0].as_os_str().len());
        sender.join().unwrap();
    }

    #[test]
    fn socket_lives_in_a_private_per_user_directory() {
        let tmp = PathBuf::from("/tmp");
        let (dir, private) = socket_dir(None, tmp.clone(), 1234);
        assert_eq!((dir, private), (PathBuf::from("/tmp/md-tex-1234"), true));

        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let uid = unsafe { libc::geteuid() };
        // The runtime directory is used when it is private to this user, as it should be.
        let run = base.path().join("run");
        std::fs::DirBuilder::new().mode(0o700).create(&run).unwrap();
        assert_eq!(
            socket_dir(Some(run.clone()), tmp.clone(), uid),
            (run.clone(), false)
        );
        // Someone else's, one others can enter, or a missing one is passed over.
        let fallback = (tmp.join(format!("md-tex-{uid}")), true);
        let theirs = (tmp.join(format!("md-tex-{}", uid + 1)), true);
        assert_eq!(socket_dir(Some(run.clone()), tmp.clone(), uid + 1), theirs);
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(socket_dir(Some(run.clone()), tmp.clone(), uid), fallback);
        let missing = base.path().join("missing");
        assert_eq!(socket_dir(Some(missing), tmp, uid), fallback);
        let mine = base.path().join("md-tex-x");
        private_dir(&mine, uid).unwrap();
        let mode = std::fs::metadata(&mine).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        private_dir(&mine, uid).unwrap();
        assert!(private_dir(&mine, uid + 1).is_err(), "someone else's");
        std::fs::set_permissions(&mine, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&mine, uid).is_err(), "others can enter");
        let link = base.path().join("link");
        let real = base.path().join("real");
        private_dir(&real, uid).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(private_dir(&link, uid).is_err(), "a symlink");
    }

    #[test]
    fn live_socket_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        let _live = UnixListener::bind(&socket).unwrap();
        assert!(matches!(claim(&socket, &[]).unwrap(), Claim::Forwarded));
        assert!(UnixStream::connect(&socket).is_ok(), "still the primary's");
    }

    #[test]
    fn stale_socket_is_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("app.sock");
        // A crashed primary leaves its socket file behind with nobody listening.
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert!(matches!(claim(&socket, &[]).unwrap(), Claim::Primary(_)));
    }
}
