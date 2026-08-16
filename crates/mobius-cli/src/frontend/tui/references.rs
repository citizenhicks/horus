//! Generic composer-reference token parsing and replacement.

use std::ops::Range;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReferenceToken {
    pub(super) trigger: char,
    pub(super) range: Range<usize>,
    pub(super) query: String,
}

/// Returns the active whitespace-delimited reference for `trigger`.
pub(super) fn active_reference_token(
    input: &str,
    cursor: usize,
    trigger: char,
) -> Option<ReferenceToken> {
    if cursor > input.len() || !input.is_char_boundary(cursor) {
        return None;
    }

    let start = input[..cursor]
        .char_indices()
        .rfind(|(_, character)| character.is_whitespace())
        .map_or(0, |(index, character)| index + character.len_utf8());
    let end = if input[cursor..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        cursor
    } else {
        input[cursor..]
            .char_indices()
            .find(|(_, character)| character.is_whitespace())
            .map_or(input.len(), |(index, _)| cursor + index)
    };
    let query = input.get(start..end)?.strip_prefix(trigger)?;

    Some(ReferenceToken {
        trigger,
        range: start..end,
        query: query.to_owned(),
    })
}

/// Replaces an unchanged active token and returns the new UTF-8 byte cursor.
pub(super) fn replace_reference_token(
    input: &mut String,
    token: &ReferenceToken,
    replacement: &str,
) -> Option<usize> {
    if input
        .get(token.range.clone())?
        .strip_prefix(token.trigger)?
        != token.query
    {
        return None;
    }
    let cursor = token.range.start + replacement.len();
    input.replace_range(token.range.clone(), replacement);
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_the_active_utf8_reference() {
        let mut input = "é read @old/path then @other".to_owned();
        let token = active_reference_token(&input, 10, '@').expect("active token");
        let cursor = replace_reference_token(&mut input, &token, "\"docs/my file.md\"").unwrap();

        assert_eq!(input, "é read \"docs/my file.md\" then @other");
        assert_eq!(&input[..cursor], "é read \"docs/my file.md\"");
        assert!(active_reference_token("email a@b.test", 10, '@').is_none());
    }
}
