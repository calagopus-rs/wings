use colored::Colorize;

pub enum LoggerLevel {
    Info,
    Debug,
    Error,
}

#[inline]
pub fn log(level: LoggerLevel, message: String) {
    if matches!(level, LoggerLevel::Debug) && !unsafe { crate::config::DEBUG } {
        return;
    }

    let level = match level {
        LoggerLevel::Info => " INF ".on_blue(),
        LoggerLevel::Debug => " DEB ".on_green(),
        LoggerLevel::Error => " ERR ".on_red(),
    };

    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    println!("{} {} {}", time.bright_black(), level, message);
}
