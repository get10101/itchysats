use time::format_description::FormatItem;
use time::macros::format_description;

pub const EVENT_TIME_FORMAT: &[FormatItem] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
