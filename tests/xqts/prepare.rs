/// Find the index of `letter` in `text`, ignoring positions inside string literals.
///
/// Port of C# EscapedIndexOf (Form1.cs:595-623).
fn escaped_index_of(text: &str, letter: char) -> Option<usize> {
    let mut is_literal = false;
    let mut literal_char = '\0';
    for (i, ch) in text.char_indices() {
        if is_literal {
            if ch == literal_char {
                is_literal = false;
            }
        } else {
            match ch {
                '"' | '\'' => {
                    literal_char = ch;
                    is_literal = true;
                }
                _ => {
                    if ch == letter {
                        return Some(i);
                    }
                }
            }
        }
    }
    None
}

/// Preprocess XQuery text to extract a bare XPath expression.
///
/// Port of C# PrepareQueryText (Form1.cs:625-641):
/// 1. Remove "(: Kelvin sign :)" marker
/// 2. Skip past the last ":)" comment close
/// 3. Extract content between { ... } (respecting string escaping)
/// 4. Trim whitespace
pub fn prepare_query_text(text: &str) -> String {
    let mut text = text.to_string();

    // Remove Kelvin sign marker
    if let Some(idx) = text.find("(: Kelvin sign :)") {
        text = format!(
            "{}{}",
            &text[..idx],
            &text[idx + "(: Kelvin sign :)".len()..]
        );
    }

    // Skip past last ":)" comment close
    if let Some(idx) = text.rfind(":)") {
        text = text[idx + 2..].to_string();
    }

    // Extract content between { ... }
    if let Some(open) = escaped_index_of(&text, '{') {
        if let Some(close) = text.rfind('}') {
            if close > open {
                text = text[open + 1..close].to_string();
            }
        }
    }

    text.trim().to_string()
}

// Self-test for prepare module (run via driver's --self-test or manually)
#[allow(dead_code)]
pub fn self_test() {
    assert_eq!(escaped_index_of("hello{world", '{'), Some(5));
    assert_eq!(escaped_index_of("'hello{world'}", '}'), Some(13));
    assert_eq!(escaped_index_of("hello world", '{'), None);

    assert_eq!(prepare_query_text("(: comment :)\n{ 1 + 2 }"), "1 + 2");
    assert_eq!(
        prepare_query_text("(: Kelvin sign :)(: test :)\n{ 42 }"),
        "42"
    );
    assert_eq!(prepare_query_text("(: comment :)\n1 + 2"), "1 + 2");
}
