//! Provider-neutral desktop operations. This is an internal backend seam, not
//! a new kernel port: an out-of-process tool provider uses the ordinary events.
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: String,
    pub application: String,
    pub title: String,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Click {
        x: u32,
        y: u32,
        #[serde(default)]
        button: Button,
        #[serde(default = "one")]
        count: u8,
    },
    Type {
        text: String,
    },
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    Scroll {
        x: u32,
        y: u32,
        direction: Direction,
        #[serde(default = "one")]
        amount: u8,
    },
    Drag {
        from_x: u32,
        from_y: u32,
        to_x: u32,
        to_y: u32,
    },
}
fn one() -> u8 {
    1
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    #[default]
    Left,
    Right,
    Middle,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    Command,
    Control,
    Shift,
    Option,
}

impl Action {
    pub fn validate(&self) -> Result<(), Failure> {
        match self {
            Self::Click { count, .. } if !(1..=3).contains(count) => {
                Err(Failure::new("click count must be between 1 and 3"))
            }
            Self::Scroll { amount, .. } if !(1..=50).contains(amount) => {
                Err(Failure::new("scroll amount must be between 1 and 50"))
            }
            Self::Type { text }
                if text.len() > 8192
                    || text
                        .chars()
                        .any(|c| c.is_control() && c != '\n' && c != '\t') =>
            {
                Err(Failure::new(
                    "text exceeds 8 KiB or contains unsupported control characters",
                ))
            }
            Self::Key { key, modifiers } => {
                let key = key.to_ascii_lowercase();
                let ordinary = key.len() == 1 && key.bytes().all(|b| b.is_ascii_alphanumeric());
                let named = [
                    "enter",
                    "tab",
                    "escape",
                    "backspace",
                    "delete",
                    "up",
                    "down",
                    "left",
                    "right",
                    "home",
                    "end",
                    "pageup",
                    "pagedown",
                    "space",
                ]
                .contains(&key.as_str());
                if (!ordinary && !named) || modifiers.len() > 4 {
                    return Err(Failure::new("unsupported key or modifier combination"));
                }
                // These are system-wide operations, not input to a selected window.
                if (modifiers.contains(&Modifier::Command)
                    && ["tab", "space", "escape"].contains(&key.as_str()))
                    || (modifiers.contains(&Modifier::Command)
                        && modifiers.contains(&Modifier::Control)
                        && key == "q")
                {
                    return Err(Failure::new(
                        "system-wide shortcuts are outside the selected desktop target",
                    ));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub fn validate_frame(&self, width: u32, height: u32) -> Result<(), Failure> {
        self.validate()?;
        let inside = |x: u32, y: u32| x < width && y < height;
        let valid = match self {
            Self::Click { x, y, .. } | Self::Scroll { x, y, .. } => inside(*x, *y),
            Self::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
            } => inside(*from_x, *from_y) && inside(*to_x, *to_y),
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(Failure::new(
                "coordinates are outside the last observed target image; observe again",
            ))
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub message: String,
    pub interrupted: bool,
    pub may_have_run: bool,
}
impl Failure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            interrupted: false,
            may_have_run: false,
        }
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// Backends own native handles, launch details and coordinate conversion. Tool
/// consumers see only opaque targets, image pixels and these ordinary actions.
pub trait DesktopDriver: Send {
    fn targets(&mut self, cancel: &CancellationToken) -> Result<Vec<Target>, Failure>;
    fn observe(&mut self, target: &str, cancel: &CancellationToken) -> Result<Frame, Failure>;
    fn act(
        &mut self,
        target: &str,
        action: &Action,
        cancel: &CancellationToken,
    ) -> Result<(), Failure>;
    /// Releases our driver resources, NEVER closes or kills the user's app.
    fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn action_contract_is_strict_and_coordinates_are_image_local() {
        for bad in [
            json!({"type":"shell","command":"x"}),
            json!({"type":"click","x":-1,"y":0}),
            json!({"type":"type","text":"x","pid":42}),
        ] {
            assert!(serde_json::from_value::<Action>(bad).is_err());
        }
        let mut click = Action::Click {
            x: 9,
            y: 19,
            button: Button::Left,
            count: 1,
        };
        assert!(click.validate_frame(10, 20).is_ok());
        click = Action::Click {
            x: 10,
            y: 19,
            button: Button::Left,
            count: 1,
        };
        assert!(click.validate_frame(10, 20).is_err());
        assert!(Action::Drag {
            from_x: 0,
            from_y: 0,
            to_x: 9,
            to_y: 20
        }
        .validate_frame(10, 20)
        .is_err());
        assert!(Action::Type {
            text: "x".repeat(8193)
        }
        .validate()
        .is_err());
        assert!(Action::Key {
            key: "space".into(),
            modifiers: vec![Modifier::Command]
        }
        .validate()
        .is_err());
        assert!(Action::Key {
            key: "a".into(),
            modifiers: vec![Modifier::Command]
        }
        .validate()
        .is_ok());
    }
}
