//! Builders for WebDriver input action sequences.
//!
//! Coordinates are CSS pixels relative to the viewport's top-left corner.

use serde_json::{Value, json};

/// A left click at viewport position (`x`, `y`).
///
/// ```ignore
/// let actions = click_at(120, 48);
/// ```
pub fn click_at(x: i64, y: i64) -> Value {
    json!([{
        "type": "pointer",
        "id": "mouse",
        "parameters": { "pointerType": "mouse" },
        "actions": [
            { "type": "pointerMove", "duration": 0, "origin": "viewport", "x": x, "y": y },
            { "type": "pointerDown", "button": 0 },
            { "type": "pointerUp", "button": 0 },
        ],
    }])
}

/// Typing `text` into whatever has focus, one key press per character.
pub fn type_text(text: &str) -> Value {
    let actions: Vec<Value> = text
        .chars()
        .flat_map(|c| {
            let key = c.to_string();
            [
                json!({ "type": "keyDown", "value": key }),
                json!({ "type": "keyUp", "value": key }),
            ]
        })
        .collect();
    keyboard(actions)
}

/// Pressing a key or key combination such as `Enter`, `Control+a` or
/// `Shift+Tab`: modifiers go down in order, the key is pressed, then the
/// modifiers are released in reverse.
pub fn press_key(combination: &str) -> Result<Value, String> {
    let keys = combination
        .split('+')
        .map(|name| key_value(name.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut actions: Vec<Value> = keys
        .iter()
        .map(|key| json!({ "type": "keyDown", "value": key }))
        .collect();
    actions.extend(
        keys.iter()
            .rev()
            .map(|key| json!({ "type": "keyUp", "value": key })),
    );
    Ok(keyboard(actions))
}

fn keyboard(actions: Vec<Value>) -> Value {
    json!([{ "type": "key", "id": "keyboard", "actions": actions }])
}

/// The WebDriver key value for a key name: a named key's code point, or a
/// single character as itself.
fn key_value(name: &str) -> Result<String, String> {
    let named = match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => '\u{E007}',
        "tab" => '\u{E004}',
        "escape" | "esc" => '\u{E00C}',
        "backspace" => '\u{E003}',
        "delete" => '\u{E017}',
        "space" => ' ',
        "arrowleft" | "left" => '\u{E012}',
        "arrowup" | "up" => '\u{E013}',
        "arrowright" | "right" => '\u{E014}',
        "arrowdown" | "down" => '\u{E015}',
        "home" => '\u{E011}',
        "end" => '\u{E010}',
        "pageup" => '\u{E00E}',
        "pagedown" => '\u{E00F}',
        "shift" => '\u{E008}',
        "control" | "ctrl" => '\u{E009}',
        "alt" => '\u{E00A}',
        "meta" | "super" => '\u{E03D}',
        _ => {
            let mut chars = name.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(c.to_string()),
                _ => Err(format!("unknown key \"{name}\"")),
            };
        }
    };
    Ok(named.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_steps(actions: &Value) -> Vec<(String, String)> {
        actions[0]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["type"].as_str().unwrap().to_owned(),
                    a["value"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn a_click_moves_then_presses_and_releases() {
        let actions = click_at(10, 20);
        let steps = &actions[0]["actions"];
        assert_eq!(steps[0]["x"], 10);
        assert_eq!(steps[0]["y"], 20);
        assert_eq!(steps[1]["type"], "pointerDown");
        assert_eq!(steps[2]["type"], "pointerUp");
    }

    #[test]
    fn typed_text_presses_each_character() {
        let steps = key_steps(&type_text("hé"));
        assert_eq!(
            steps,
            [
                ("keyDown".into(), "h".into()),
                ("keyUp".into(), "h".into()),
                ("keyDown".into(), "é".into()),
                ("keyUp".into(), "é".into()),
            ]
        );
    }

    #[test]
    fn a_combination_releases_modifiers_last() {
        let steps = key_steps(&press_key("Control+a").unwrap());
        assert_eq!(
            steps,
            [
                ("keyDown".into(), "\u{E009}".into()),
                ("keyDown".into(), "a".into()),
                ("keyUp".into(), "a".into()),
                ("keyUp".into(), "\u{E009}".into()),
            ]
        );
    }

    #[test]
    fn named_keys_are_case_insensitive() {
        assert_eq!(key_value("enter").unwrap(), "\u{E007}");
        assert_eq!(key_value("ArrowDown").unwrap(), "\u{E015}");
    }

    #[test]
    fn an_unknown_key_name_is_rejected() {
        assert!(press_key("Hyper").is_err());
        assert!(press_key("Control+").is_err());
    }
}
