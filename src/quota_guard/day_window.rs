//! 日账本使用保存的 IANA 时区；本地日可能因夏令时长于或短于 24 小时。
use chrono::{DateTime, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::{GapInfo, Tz};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DayWindow {
    pub start_ms: i64,
    pub end_ms: i64,
    pub timezone: Tz,
}

fn midnight(date: NaiveDate, timezone: Tz) -> Option<DateTime<Tz>> {
    let local = date.and_hms_opt(0, 0, 0)?;
    match timezone.from_local_datetime(&local) {
        LocalResult::Single(value) => Some(value),
        // 午夜重叠时取首次午夜；午夜缺失时取该日期的首个有效时刻。
        LocalResult::Ambiguous(first, second) => Some(first.min(second)),
        LocalResult::None => GapInfo::new(&local, &timezone)?.end,
    }
}

impl DayWindow {
    pub fn at(now_ms: i64, timezone: Tz) -> Option<Self> {
        let date = DateTime::<Utc>::from_timestamp_millis(now_ms)?
            .with_timezone(&timezone)
            .date_naive();
        let start_ms = midnight(date, timezone)?.timestamp_millis();
        let end_ms = midnight(date.succ_opt()?, timezone)?.timestamp_millis();
        (start_ms <= now_ms && now_ms < end_ms).then_some(Self {
            start_ms,
            end_ms,
            timezone,
        })
    }
}

/// 用户确认的当前日累计金额；只追加此后开始的请求，避免历史迟记费用重复计算。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyCalibration {
    pub amount_usd: String,
    pub at_ms: i64,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub timezone: Tz,
}

impl DailyCalibration {
    pub fn new(amount: &str, now_ms: i64, timezone: Tz) -> Result<Self, String> {
        let amount_usd = super::daily::Usd::limit(amount)
            .ok_or("校准金额必须为非负十进制字符串，最多 18 位整数和 12 位小数")?
            .text();
        let window = DayWindow::at(now_ms, timezone).ok_or("无法确定当前时区的日记账窗口")?;
        Ok(Self {
            amount_usd,
            at_ms: now_ms,
            window_start_ms: window.start_ms,
            window_end_ms: window.end_ms,
            timezone,
        })
    }

    pub(super) fn validate(&mut self) -> Result<(), String> {
        let expected = Self::new(&self.amount_usd, self.at_ms, self.timezone)?;
        if expected.window_start_ms != self.window_start_ms
            || expected.window_end_ms != self.window_end_ms
        {
            return Err("校准时间与日记账窗口不一致".into());
        }
        self.amount_usd = expected.amount_usd;
        Ok(())
    }

    pub(super) fn active_in(&self, window: DayWindow, now_ms: i64) -> bool {
        self.timezone == window.timezone
            && self.window_start_ms == window.start_ms
            && self.window_end_ms == window.end_ms
            && self.at_ms >= window.start_ms
            && self.at_ms < window.end_ms
            && self.at_ms <= now_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn timezone_changes_the_actual_query_window() {
        let now = ms("2026-11-07T12:00:00Z");
        let utc = DayWindow::at(now, chrono_tz::UTC).unwrap();
        let shanghai = DayWindow::at(now, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(utc.start_ms, ms("2026-11-07T00:00:00Z"));
        assert_eq!(shanghai.start_ms, ms("2026-11-06T16:00:00Z"));
        assert_eq!(shanghai.end_ms, ms("2026-11-07T16:00:00Z"));
        assert!(DayWindow::at(i64::MAX, chrono_tz::UTC).is_none());
    }

    #[test]
    fn dst_days_use_local_midnight_not_a_fixed_offset_or_twenty_four_hours() {
        for (at, hours, start, end) in [
            (
                "2026-03-08T12:00:00Z",
                23,
                "2026-03-08T05:00:00Z",
                "2026-03-09T04:00:00Z",
            ),
            (
                "2026-11-01T12:00:00Z",
                25,
                "2026-11-01T04:00:00Z",
                "2026-11-02T05:00:00Z",
            ),
        ] {
            let window = DayWindow::at(ms(at), chrono_tz::America::Toronto).unwrap();
            assert_eq!(window.start_ms, ms(start));
            assert_eq!(window.end_ms, ms(end));
            assert_eq!(window.end_ms - window.start_ms, hours * 3_600_000);
        }
    }

    #[test]
    fn midnight_gaps_and_skipped_dates_have_valid_boundaries() {
        let gap = DayWindow::at(ms("2018-11-04T12:00:00Z"), chrono_tz::America::Sao_Paulo).unwrap();
        assert_eq!(gap.start_ms, ms("2018-11-04T03:00:00Z"));
        assert_eq!(gap.end_ms, ms("2018-11-05T02:00:00Z"));
        let skipped = DayWindow::at(ms("2011-12-29T12:00:00Z"), chrono_tz::Pacific::Apia).unwrap();
        assert_eq!(skipped.start_ms, ms("2011-12-29T10:00:00Z"));
        assert_eq!(skipped.end_ms, ms("2011-12-30T10:00:00Z"));
    }

    #[test]
    fn calibration_expires_at_the_local_boundary_and_cannot_cross_timezones() {
        let at = ms("2026-11-07T12:00:00Z");
        let calibration = DailyCalibration::new("050.8900", at, chrono_tz::Asia::Shanghai).unwrap();
        assert_eq!(calibration.amount_usd, "50.89");
        assert!(calibration.active_in(DayWindow::at(at, chrono_tz::Asia::Shanghai).unwrap(), at));
        assert!(!calibration.active_in(DayWindow::at(at, chrono_tz::UTC).unwrap(), at));
        let end = calibration.window_end_ms;
        assert!(
            !calibration.active_in(DayWindow::at(end, chrono_tz::Asia::Shanghai).unwrap(), end)
        );
    }
}
