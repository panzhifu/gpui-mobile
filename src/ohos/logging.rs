//! HarmonyOS / OpenHarmony logging via the `hilog` NDK API.
//!
//! OpenHarmony routes native logs through `libhilog_ndk.z.so`.  The C entry
//! point is:
//!
//! ```c
//! int OH_LOG_Print(LogType type, LogLevel level, unsigned int domain,
//!                  const char *tag, const char *fmt, ...);
//! ```
//!
//! `OH_LOG_Print` takes a **printf-style** format string, so we always pass
//! a literal `"%{public}s"` format followed by the message as a `char *`.
//! `%{public}` avoids the default privacy redaction (plain `%s` is logged as
//! `<private>` on OpenHarmony release builds).
//!
//! This module is compiled only for `target_env = "ohos"`.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::sync::OnceLock;

// ── hilog C ABI ──────────────────────────────────────────────────────────────

/// `LogType` —— API 26 的 `hilog/log.h` 里这个枚举**只有** `LOG_APP = 0`
/// （旧 OpenHarmony 那份 `0=MIN / 1=APP / 2=CORE` 的编号已经不作数了）。
/// 传 1 是越界的 LogType，`OH_LOG_Print` 会把每一条都丢掉：模拟器实测 native 侧
/// 一行 hilog 都不出，而同一进程的 ArkTS `hilog.info` 正常。
const LOG_APP: c_int = 0;

/// `LogLevel`（`hilog/log.h`：DEBUG=3 / INFO=4 / WARN=5 / ERROR=6 / FATAL=7）。
const LOG_DEBUG: c_int = 3;
const LOG_INFO: c_int = 4;
const LOG_WARN: c_int = 5;
const LOG_ERROR: c_int = 6;

/// Application log domain.  The value is arbitrary for user apps but must be
/// stable so that `hdc shell hilog -D <domain>` filtering works.
const LOG_DOMAIN: u32 = 0x0;

/// Tag shown in the hilog output.
const LOG_TAG: &[u8] = b"gpui-ohos\0";

unsafe extern "C" {
    /// `int OH_LOG_Print(LogType type, LogLevel level, unsigned int domain,
    ///                   const char *tag, const char *fmt, ...)`
    ///
    /// 声明成**变参**而不是定参：C 那头是 `...`，按定参声明时调用方不写 x86_64
    /// 变参调用约定的寄存器保存区（`al` = 向量寄存器数），被调方的 `va_start`
    /// 读到的就是未定义的内容。
    fn OH_LOG_Print(
        type_: c_int,
        level: c_int,
        domain: u32,
        tag: *const c_char,
        fmt: *const c_char,
        ...
    ) -> c_int;
}

/// Print a single line through hilog, best-effort (errors are ignored).
fn hilog(level: c_int, msg: &str) {
    // hilog rejects over-long lines; truncate defensively at a UTF-8 boundary.
    let mut text = msg;
    if text.len() > 3000 {
        let mut end = 3000;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text = &text[..end];
    }

    let Ok(c_msg) = CString::new(text) else {
        return;
    };
    // `%{public}s` prints the argument without privacy redaction.
    let fmt = c"%{public}s";
    unsafe {
        OH_LOG_Print(
            LOG_APP,
            level,
            LOG_DOMAIN,
            LOG_TAG.as_ptr() as *const c_char,
            fmt.as_ptr(),
            c_msg.as_ptr(),
        );
    }
}

// ── log::Log implementation ──────────────────────────────────────────────────

struct HilogLogger;

impl log::Log for HilogLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let level = match record.level() {
            log::Level::Error => LOG_ERROR,
            log::Level::Warn => LOG_WARN,
            log::Level::Info => LOG_INFO,
            log::Level::Debug | log::Level::Trace => LOG_DEBUG,
        };
        let line = format!(
            "[{}] {}: {}",
            record.level(),
            record.target(),
            record.args()
        );
        hilog(level, &line);
    }

    fn flush(&self) {}
}

static LOGGER: HilogLogger = HilogLogger;
static INIT: OnceLock<()> = OnceLock::new();

/// Initialise the hilog logger.  Subsequent calls are no-ops.
pub fn init_logger() {
    INIT.get_or_init(|| {
        let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
        hilog(LOG_INFO, "gpui-ohos logger initialised");
    });
}
