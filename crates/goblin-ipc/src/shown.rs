//! Which characters a card cannot show for what they are.
//!
//! The owner gives leave only for what a card showed them whole. Some characters are in the
//! text and not on the screen: they draw nothing, or they look like the plain space and are
//! not it. A command with one of them in it does not read as it will run. The daemon takes
//! these out of a Hermes's question (which marks the question as cut), and the app refuses to
//! call a card whole that has one, so the two go by the same list, kept here.
//!
//! The list is not all a client needs: what a font has no glyph for, or draws as nothing,
//! only the client that draws can know.

/// Whether `c`, coming after `before`, is one a reader of a card does not see for what it is:
///
/// - a control character other than a line break (a tab is drawn as a space);
/// - a space that is not the plain space (no-break, the fixed-width ones, the ideographic
///   one): it reads as a space where a shell sees part of a word;
/// - a format character, or one a renderer is told to ignore: soft hyphen, zero-width and
///   joining characters, direction marks and overrides, invisible operators, fillers, the
///   combining grapheme joiner, tag characters;
/// - a variation selector, unless it is the one that follows a symbol to say whether to
///   draw it as an emoji (it then changes nothing but that symbol's look).
pub fn unseen(before: Option<char>, c: char) -> bool {
    match c {
        '\n' => false,
        '\u{fe0e}' | '\u{fe0f}' => !before.is_some_and(takes_emoji_selector),
        c if c.is_control() => true,
        // Spaces that are not U+0020, and the line and paragraph separators.
        '\u{a0}'
        | '\u{1680}'
        | '\u{2000}'..='\u{200a}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{202f}'
        | '\u{205f}'
        | '\u{3000}' => true,
        // Format characters (general category Cf) and the code points Unicode says to
        // ignore when there is no special handling for them (Default_Ignorable_Code_Point).
        '\u{ad}'
        | '\u{34f}'
        | '\u{600}'..='\u{605}'
        | '\u{61c}'
        | '\u{6dd}'
        | '\u{70f}'
        | '\u{890}'..='\u{891}'
        | '\u{8e2}'
        | '\u{115f}'..='\u{1160}'
        | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180f}'
        | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}'
        | '\u{3164}'
        | '\u{fe00}'..='\u{fe0d}'
        | '\u{feff}'
        | '\u{ffa0}'
        | '\u{fff0}'..='\u{fffb}'
        | '\u{110bd}'
        | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}'
        | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0000}'..='\u{e0fff}' => true,
        _ => false,
    }
}

/// Whether a variation selector after `c` is the ordinary way to ask for its emoji (or
/// text) look: `c` is a symbol from the blocks emoji come from, never a letter, a digit or
/// punctuation a command is written with.
fn takes_emoji_selector(c: char) -> bool {
    matches!(c,
        '\u{a9}' | '\u{ae}' | '\u{203c}' | '\u{2049}' | '\u{2122}' | '\u{2139}'
        | '\u{2190}'..='\u{21ff}'     // arrows
        | '\u{2300}'..='\u{23ff}'     // technical symbols
        | '\u{24c2}'
        | '\u{25a0}'..='\u{27bf}'     // shapes, symbols, dingbats
        | '\u{2900}'..='\u{297f}'     // more arrows
        | '\u{2b00}'..='\u{2bff}'     // more symbols and arrows
        | '\u{3030}' | '\u{303d}' | '\u{3297}' | '\u{3299}'
        | '\u{1f000}'..='\u{1faff}')
}

/// Whether `c` is one of the two selectors [`unseen`] lets by after a symbol. It draws
/// nothing there, and is not missed.
pub fn is_emoji_selector(c: char) -> bool {
    matches!(c, '\u{fe0e}' | '\u{fe0f}')
}

/// `text` without the characters that are [`unseen`], and whether there were any. Each
/// character is judged by what is kept before it, so the result, checked again, has none.
pub fn without_unseen(text: &str) -> (String, bool) {
    let mut kept = String::with_capacity(text.len());
    let mut before = None;
    for c in text.chars() {
        if !unseen(before, c) {
            kept.push(c);
            before = Some(c);
        }
    }
    let lost = kept.len() != text.len();
    (kept, lost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_with_ordinary_characters_is_all_seen() {
        let plain = "rm -rf build && make all\n  if x:\n    echo \"caf\u{e9} \u{2014} \u{4e2d}\u{6587}\" 'a|b' ~/x$y";
        assert_eq!(without_unseen(plain), (plain.to_owned(), false));
        // A combining accent is drawn on its letter.
        assert!(!unseen(Some('e'), '\u{301}'));
    }

    #[test]
    fn what_draws_nothing_or_passes_for_a_space_is_unseen() {
        for c in [
            '\u{34f}',   // combining grapheme joiner
            '\u{ad}',    // soft hyphen
            '\u{200b}',  // zero width space
            '\u{200d}',  // zero width joiner
            '\u{202e}',  // right-to-left override
            '\u{2060}',  // word joiner
            '\u{2063}',  // invisible separator
            '\u{feff}',  // byte order mark
            '\u{e0041}', // tag latin capital a
            '\u{e0020}', // tag space
            '\u{e0100}', // variation selector-17
            '\u{3164}',  // hangul filler
            '\u{a0}',    // no-break space
            '\u{2003}',  // em space
            '\u{2009}',  // thin space
            '\u{202f}',  // narrow no-break space
            '\u{205f}',  // medium mathematical space
            '\u{3000}',  // ideographic space
            '\u{2028}',  // line separator
            '\t',
            '\r',
            '\u{7}',
            '\u{1b}',
        ] {
            assert!(unseen(Some('a'), c), "U+{:04X}", c as u32);
            assert!(unseen(None, c), "U+{:04X} first", c as u32);
        }
        assert!(!unseen(Some('a'), ' '));
        assert!(!unseen(Some('a'), '\n'));
    }

    #[test]
    fn a_selector_is_let_by_only_on_a_symbol() {
        // As Hermes opens an approval, and as people write a heart.
        for symbol in ['\u{26a0}', '\u{2764}', '\u{2705}', '\u{1f600}', '\u{2194}'] {
            assert!(!unseen(Some(symbol), '\u{fe0f}'), "U+{:04X}", symbol as u32);
            assert!(!unseen(Some(symbol), '\u{fe0e}'), "U+{:04X}", symbol as u32);
        }
        // After a letter, a digit, punctuation, a space, another selector, or nothing, it
        // is a character in a command that the screen does not show.
        for other in [
            'a', '7', '#', '*', ']', '=', ' ', '\n', '\u{fe0f}', '\u{e9}',
        ] {
            assert!(unseen(Some(other), '\u{fe0f}'), "U+{:04X}", other as u32);
        }
        assert!(unseen(None, '\u{fe0f}'));
        // The other selectors pick between forms of a character, unseen wherever they are.
        assert!(unseen(Some('\u{26a0}'), '\u{fe00}'));
        assert!(is_emoji_selector('\u{fe0f}') && !is_emoji_selector('\u{fe00}'));
    }

    #[test]
    fn taking_them_out_says_so_and_lets_nothing_by_behind_them() {
        // The test reads as always true and is not: the second `a` has a joiner after it.
        let sly = "[ a = a\u{34f} ] || rm -rf ~/Documents";
        assert_eq!(
            without_unseen(sly),
            ("[ a = a ] || rm -rf ~/Documents".to_owned(), true)
        );
        assert_eq!(
            without_unseen("\u{26a0}\u{fe0f} ls\u{a0}-la"),
            ("\u{26a0}\u{fe0f} ls-la".to_owned(), true)
        );
        // Judged by what is kept: the selector after a letter goes, the one that follows
        // the symbol once the hidden character between them is gone stays, and a second
        // selector goes. What is left has nothing unseen in it.
        let (kept, lost) = without_unseen("a\u{fe0f}\u{26a0}\u{200b}\u{fe0f}\u{fe0f}");
        assert_eq!((kept.as_str(), lost), ("a\u{26a0}\u{fe0f}", true));
        assert_eq!(without_unseen(&kept), (kept.clone(), false));
        assert_eq!(without_unseen(""), (String::new(), false));
    }
}
