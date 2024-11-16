use chrono::{DateTime, Local, TimeZone};

const FORMAT: &str = "%Y-%m-%d %H:%M:%S%.9f";

///
/// 将毫秒转日期类型
///
pub fn second_to_date(millisecond: u64) -> DateTime<Local>{
    Local.timestamp_nanos((millisecond * 1_000_000_000) as i64)
}
///
/// 将日期类型转成字符串。 格式：2024-08-09 10:44:46.584073200
///
pub fn date_time_format(date_time:&DateTime<Local>) -> String{
    let formatted_time = date_time.format(FORMAT).to_string();
    formatted_time
}