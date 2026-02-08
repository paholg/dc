use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor};

pub const BOLD: SetAttribute = SetAttribute(Attribute::Bold);
pub const RESET: SetAttribute = SetAttribute(Attribute::Reset);

pub const BLACK: SetForegroundColor = SetForegroundColor(Color::Black);
pub const DARK_RED: SetForegroundColor = SetForegroundColor(Color::DarkRed);
pub const DARK_GREEN: SetForegroundColor = SetForegroundColor(Color::DarkGreen);
pub const DARK_YELLOW: SetForegroundColor = SetForegroundColor(Color::DarkYellow);
pub const DARK_BLUE: SetForegroundColor = SetForegroundColor(Color::DarkBlue);
pub const DARK_MAGENTA: SetForegroundColor = SetForegroundColor(Color::DarkMagenta);
pub const DARK_CYAN: SetForegroundColor = SetForegroundColor(Color::DarkCyan);
pub const GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
pub const RED: SetForegroundColor = SetForegroundColor(Color::Red);
pub const GREEN: SetForegroundColor = SetForegroundColor(Color::Green);
pub const YELLOW: SetForegroundColor = SetForegroundColor(Color::Yellow);
pub const BLUE: SetForegroundColor = SetForegroundColor(Color::Blue);
pub const MAGENTA: SetForegroundColor = SetForegroundColor(Color::Magenta);
pub const CYAN: SetForegroundColor = SetForegroundColor(Color::Cyan);
pub const LIGHT_GRAY: SetForegroundColor = SetForegroundColor(Color::Grey);
pub const WHITE: SetForegroundColor = SetForegroundColor(Color::White);
