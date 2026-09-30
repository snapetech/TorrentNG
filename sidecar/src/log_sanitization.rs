pub(crate) fn sanitize_operator_log_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() || is_formatting_control(character) {
            escaped.extend(character.escape_debug());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

fn is_formatting_control(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

#[cfg(test)]
mod tests {
    use super::sanitize_operator_log_text;

    #[test]
    fn escapes_controls_and_unicode_log_formatting_without_losing_text() {
        assert_eq!(
            sanitize_operator_log_text(
                "ok café\nforged\r\n\u{1b}[31m\u{85}\u{2028}\u{2029}\u{202e}\u{2066}"
            ),
            "ok café\\nforged\\r\\n\\u{1b}[31m\\u{85}\\u{2028}\\u{2029}\\u{202e}\\u{2066}"
        );
    }
}
