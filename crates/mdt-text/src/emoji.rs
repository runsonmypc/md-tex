//! Emoji clusters, found before layout so that they ask for the emoji font first and get
//! the advance the platform draws them with.

use std::ops::Range;

use icu_properties::{CodePointSetData, props::EmojiPresentation};

/// Characters that continue an emoji cluster: the emoji selector, keycap, skin tones and
/// tag characters (subdivision flags).
fn continues(c: char) -> bool {
    matches!(c, '\u{FE0F}' | '\u{20E3}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0020}'..='\u{E007F}')
}

fn regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Byte ranges of emoji clusters: a character shown as emoji by default, or any character
/// followed by U+FE0F (so `❤️` is the red heart, not the body font's text heart), with its
/// modifiers and ZWJ continuations. A flag is two regional indicators.
pub fn emoji_ranges(text: &str) -> Vec<Range<usize>> {
    // Every paragraph comes through here. No ASCII character is shown as emoji by default,
    // and U+FE0F is not ASCII.
    if text.is_ascii() {
        return Vec::new();
    }
    let presentation = CodePointSetData::new::<EmojiPresentation>();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let at = |j: usize| chars.get(j).map(|&(_, c)| c);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some((start, c)) = chars.get(i).copied() {
        if !presentation.contains(c) && at(i + 1) != Some('\u{FE0F}') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if regional_indicator(c) && at(j).is_some_and(regional_indicator) {
            j += 1;
        }
        loop {
            match at(j) {
                Some(n) if continues(n) => j += 1,
                Some('\u{200D}') if at(j + 1).is_some() => j += 2,
                _ => break,
            }
        }
        out.push(start..chars.get(j).map_or(text.len(), |&(b, _)| b));
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::emoji_ranges;

    fn found(text: &str) -> Vec<&str> {
        emoji_ranges(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn clusters() {
        assert_eq!(found("a 🚀 b"), ["🚀"]);
        assert_eq!(found("❤️ ❤ ☺"), ["❤️"]);
        assert_eq!(found("👍🏽👨‍👩‍👧🇯🇵🇫🇷"), ["👍🏽", "👨‍👩‍👧", "🇯🇵", "🇫🇷"]);
        assert_eq!(found("#️⃣ # 1"), ["#️⃣"]);
        assert_eq!(found("🏴󠁧󠁢󠁳󠁣󠁴󠁿!"), ["🏴󠁧󠁢󠁳󠁣󠁴󠁿"]);
        assert!(found("plain text, 日本語 — ©").is_empty());
    }
}
