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
///   one, the blank braille cell): it reads as a space where a shell sees part of a word;
/// - a format character, or one a renderer is told to ignore: soft hyphen, zero-width and
///   joining characters, direction marks and overrides, invisible operators, fillers, the
///   combining grapheme joiner, tag characters;
/// - a variation selector, wherever it stands. A screen that honours one draws the symbol
///   before it another way, and one that does not (the handheld's) draws nothing for it:
///   either way `a` and `a` with a selector can look the same and are not the same bytes;
/// - a combining mark with nothing to sit on: the first character, or one after a space or
///   a line break. It is drawn on whatever happens to be beside it.
pub fn unseen(before: Option<char>, c: char) -> bool {
    match c {
        '\n' => false,
        c if c.is_control() => true,
        // Spaces that are not U+0020, and the line and paragraph separators.
        '\u{a0}'
        | '\u{1680}'
        | '\u{2000}'..='\u{200a}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{202f}'
        | '\u{205f}'
        | '\u{2800}'
        | '\u{3000}' => true,
        // Format characters (general category Cf) and the code points Unicode says to
        // ignore when there is no special handling for them (Default_Ignorable_Code_Point),
        // the variation selectors among them.
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
        | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}'
        | '\u{ffa0}'
        | '\u{fff0}'..='\u{fffb}'
        | '\u{110bd}'
        | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}'
        | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0000}'..='\u{e0fff}' => true,
        c if is_combining(c) => !before.is_some_and(|b| !matches!(b, ' ' | '\n')),
        _ => false,
    }
}

/// Whether `c` is from the blocks of marks that are drawn on the character before them
/// (accents, enclosing marks, half marks). The marks of scripts with their own blocks are
/// not listed: a client finds those by what its font does with them.
fn is_combining(c: char) -> bool {
    matches!(c,
        '\u{300}'..='\u{36f}'
        | '\u{483}'..='\u{489}'
        | '\u{1ab0}'..='\u{1aff}'
        | '\u{1dc0}'..='\u{1dff}'
        | '\u{20d0}'..='\u{20ff}'
        | '\u{fe20}'..='\u{fe2f}')
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
            '\u{2800}',  // braille pattern blank
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
    fn a_selector_is_unseen_wherever_it_stands() {
        // After a letter, a digit, punctuation, a space, nothing, and after the symbols
        // that are written with one: the screen draws the same thing with it and without.
        for before in [
            'a',
            '7',
            ']',
            ' ',
            '\n',
            '\u{26a0}',
            '\u{2713}',
            '\u{2764}',
            '\u{1f600}',
        ] {
            for selector in ['\u{fe0f}', '\u{fe0e}', '\u{fe00}'] {
                assert!(
                    unseen(Some(before), selector),
                    "U+{:04X} after U+{:04X}",
                    selector as u32,
                    before as u32
                );
            }
        }
        assert!(unseen(None, '\u{fe0f}'));
        // The two ticks look the same and are not: the test is false, and the rm runs.
        let sly = "[ \u{2713} = \u{2713}\u{fe0f} ] || rm -rf ~/Documents";
        assert_eq!(
            without_unseen(sly),
            (
                "[ \u{2713} = \u{2713} ] || rm -rf ~/Documents".to_owned(),
                true
            )
        );
    }

    #[test]
    fn a_combining_mark_needs_a_character_to_sit_on() {
        for mark in ['\u{301}', '\u{338}', '\u{20e3}', '\u{fe20}'] {
            assert!(
                !unseen(Some('e'), mark),
                "U+{:04X} on a letter",
                mark as u32
            );
            assert!(!unseen(Some('='), mark), "U+{:04X} on a sign", mark as u32);
            assert!(
                !unseen(Some('\u{301}'), mark),
                "U+{:04X} on a mark",
                mark as u32
            );
            assert!(unseen(None, mark), "U+{:04X} first", mark as u32);
            assert!(
                unseen(Some(' '), mark),
                "U+{:04X} after a space",
                mark as u32
            );
            assert!(
                unseen(Some('\n'), mark),
                "U+{:04X} after a line break",
                mark as u32
            );
        }
        // One left without its letter by what was taken out before it goes too.
        assert_eq!(
            without_unseen("a \u{200b}\u{301}b e\u{301}"),
            ("a b e\u{301}".to_owned(), true)
        );
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
            ("\u{26a0} ls-la".to_owned(), true)
        );
        // What is left has nothing unseen in it.
        let (kept, lost) = without_unseen("a\u{fe0f}\u{26a0}\u{200b}\u{fe0f} \u{301}x");
        assert_eq!((kept.as_str(), lost), ("a\u{26a0} x", true));
        assert_eq!(without_unseen(&kept), (kept.clone(), false));
        assert_eq!(without_unseen(""), (String::new(), false));
    }
}
