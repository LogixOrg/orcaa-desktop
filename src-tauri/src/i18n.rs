//! Strings for the native chrome — tray menu, update toasts, tray hint.
//!
//! The web app owns its own translations, but everything Rust draws sits
//! *outside* the webview and can't reach them. Those strings live here so the
//! desktop shell obeys the same rule as the rest of the platform: nothing
//! user-visible is hardcoded in one language.
//!
//! Every accessor returns a value for **both** English and Arabic — the `match`
//! is exhaustive, so a half-translated string is a compile error rather than a
//! raw key leaking into someone's system tray.

use sys_locale::get_locale;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Ar,
}

impl Lang {
    /// Follows the OS display language. The webview picks its own locale from
    /// the user's account, but the tray is drawn before any page loads and
    /// before we know who is logging in — the OS is the only signal available
    /// that early, and it's the one every other native app uses too.
    fn detect() -> Self {
        match get_locale() {
            Some(tag) if tag.to_ascii_lowercase().starts_with("ar") => Lang::Ar,
            _ => Lang::En,
        }
    }
}

/// Localized native-chrome strings, bound to the running app's product name.
#[derive(Clone, Debug)]
pub struct Strings {
    lang: Lang,
    product: String,
}

impl Strings {
    pub fn detect(product: String) -> Self {
        Self {
            lang: Lang::detect(),
            product,
        }
    }

    fn pick(&self, en: &'static str, ar: &'static str) -> String {
        match self.lang {
            Lang::En => en.to_string(),
            Lang::Ar => ar.to_string(),
        }
    }

    // ---- Tray menu ----------------------------------------------------

    pub fn tray_open(&self) -> String {
        match self.lang {
            Lang::En => format!("Open {}", self.product),
            Lang::Ar => format!("فتح {}", self.product),
        }
    }

    pub fn tray_check_updates(&self) -> String {
        self.pick("Check for Updates…", "البحث عن تحديثات…")
    }

    pub fn tray_quit(&self) -> String {
        self.pick("Quit", "إنهاء")
    }

    // ---- Tray quick actions -------------------------------------------

    pub fn tray_pos(&self) -> String {
        self.pick("Point of Sale", "نقطة البيع")
    }

    pub fn tray_dashboard(&self) -> String {
        self.pick("Dashboard", "لوحة التحكم")
    }

    pub fn tray_today(&self) -> String {
        self.pick("Today", "اليوم")
    }

    // ---- Window controls (the shell pages' own titlebar on Windows) ----

    pub fn window_minimize(&self) -> String {
        self.pick("Minimize", "تصغير")
    }

    pub fn window_maximize(&self) -> String {
        self.pick("Maximize", "تكبير")
    }

    pub fn window_restore(&self) -> String {
        self.pick("Restore", "استعادة")
    }

    pub fn window_close(&self) -> String {
        self.pick("Close", "إغلاق")
    }

    // ---- Hide-to-tray hint (shown once, the first time X is pressed) ---

    pub fn tray_hint_title(&self) -> String {
        self.pick("Still running", "لا يزال قيد التشغيل")
    }

    pub fn tray_hint_body(&self) -> String {
        match self.lang {
            Lang::En => format!(
                "{} is still running in the system tray so notifications keep arriving. Right-click the tray icon to quit.",
                self.product
            ),
            Lang::Ar => format!(
                "لا يزال {} يعمل في شريط النظام حتى تستمر الإشعارات في الوصول. انقر بزر الفأرة الأيمن على الأيقونة للإنهاء.",
                self.product
            ),
        }
    }

    // ---- Browser sign-in holding page ---------------------------------

    /// `rtl` drives both the document direction and the layout of the holding
    /// page, so Arabic doesn't render as mirrored-looking English.
    pub fn is_rtl(&self) -> bool {
        self.lang == Lang::Ar
    }

    pub fn html_lang(&self) -> &'static str {
        match self.lang {
            Lang::En => "en",
            Lang::Ar => "ar",
        }
    }

    pub fn signin_welcome_title(&self) -> String {
        match self.lang {
            Lang::En => format!("Welcome to {}", self.product),
            Lang::Ar => format!("مرحباً بك في {}", self.product),
        }
    }

    pub fn signin_welcome_body(&self) -> String {
        self.pick(
            "Sign in or create your account to get started. We'll open your browser so your password manager and Google sign-in work normally.",
            "سجّل الدخول أو أنشئ حسابك للبدء. سنفتح متصفحك ليعمل مدير كلمات المرور وتسجيل الدخول بجوجل كالمعتاد.",
        )
    }

    pub fn signin_cta(&self) -> String {
        self.pick("Log in or sign up", "تسجيل الدخول أو إنشاء حساب")
    }

    pub fn signin_title(&self) -> String {
        self.pick("Continue in your browser", "أكمِل من المتصفح")
    }

    pub fn signin_body(&self) -> String {
        match self.lang {
            Lang::En => format!(
                "We opened {} sign-in in your browser. Finish signing in there and this window will pick up automatically.",
                self.product
            ),
            Lang::Ar => format!(
                "فتحنا صفحة تسجيل الدخول إلى {} في متصفحك. أكمِل تسجيل الدخول هناك وستتابع هذه النافذة تلقائيًا.",
                self.product
            ),
        }
    }

    pub fn signin_retry(&self) -> String {
        self.pick("Open sign-in again", "افتح تسجيل الدخول مرة أخرى")
    }

    // ---- Boot page (shown from the first frame, before any network) ----

    pub fn loading_title(&self) -> String {
        match self.lang {
            Lang::En => format!("Starting {}", self.product),
            Lang::Ar => format!("جارٍ تشغيل {}", self.product),
        }
    }

    pub fn loading_body(&self) -> String {
        self.pick(
            "One moment while we connect you.",
            "لحظة من فضلك، جارٍ الاتصال.",
        )
    }

    // ---- Offline / unreachable backend --------------------------------

    pub fn offline_title(&self) -> String {
        match self.lang {
            Lang::En => format!("Can't reach {}", self.product),
            Lang::Ar => format!("تعذّر الوصول إلى {}", self.product),
        }
    }

    pub fn offline_body(&self) -> String {
        self.pick(
            "Check your internet connection and try again. Everything you were working on is still here.",
            "تحقّق من اتصالك بالإنترنت وحاول مرة أخرى. كل ما كنت تعمل عليه لا يزال موجودًا.",
        )
    }

    pub fn offline_retry(&self) -> String {
        self.pick("Try again", "إعادة المحاولة")
    }

    // ---- Incoming-call toast buttons ----------------------------------

    pub fn call_answer(&self) -> String {
        self.pick("Answer", "الرد")
    }

    pub fn call_decline(&self) -> String {
        self.pick("Decline", "رفض")
    }

    // ---- Downloads ----------------------------------------------------

    pub fn download_done_title(&self) -> String {
        self.pick("Download complete", "اكتمل التنزيل")
    }

    pub fn download_reveal(&self) -> String {
        self.pick("Show in folder", "إظهار في المجلد")
    }

    pub fn download_failed_title(&self) -> String {
        self.pick("Download failed", "فشل التنزيل")
    }

    // ---- Tray: start with the system ----------------------------------

    /// Named the way each platform names it in its own settings, so the item
    /// reads like something the OS offers rather than something we invented.
    pub fn tray_autostart(&self) -> String {
        match self.lang {
            Lang::En => if cfg!(target_os = "macos") {
                "Open at Login"
            } else if cfg!(windows) {
                "Start with Windows"
            } else {
                "Start on login"
            }
            .to_string(),
            Lang::Ar => "التشغيل عند بدء النظام".to_string(),
        }
    }

    // ---- First-run autostart offer -------------------------------------

    pub fn autostart_prompt_title(&self) -> String {
        self.pick("Start with your computer?", "التشغيل مع بدء تشغيل جهازك؟")
    }

    pub fn autostart_prompt_body(&self) -> String {
        match self.lang {
            Lang::En => format!(
                "Let {} start when you sign in — tucked away in the tray — so bookings, messages and calls reach you all day. You can change this anytime from the tray icon.",
                self.product
            ),
            Lang::Ar => format!(
                "اسمح لـ {} بالبدء عند تسجيل دخولك — في شريط النظام — لتصلك الحجوزات والرسائل والمكالمات طوال اليوم. يمكنك تغيير ذلك في أي وقت من أيقونة شريط النظام.",
                self.product
            ),
        }
    }

    pub fn autostart_prompt_not_now(&self) -> String {
        self.pick("Not now", "ليس الآن")
    }

    // ---- Updater ------------------------------------------------------

    // ---- Update prompt (asks before installing) -----------------------

    pub fn update_prompt_title(&self) -> String {
        match self.lang {
            Lang::En => format!("A new version of {} is available", self.product),
            Lang::Ar => format!("يتوفر إصدار جديد من {}", self.product),
        }
    }

    /// Shows both versions so the user can see exactly what changes.
    pub fn update_prompt_body(&self, current: &str, next: &str) -> String {
        match self.lang {
            Lang::En => format!(
                "{current} → {next}\n\nWould you like to install it now? {} will restart and take you back to where you left off.",
                self.product
            ),
            Lang::Ar => format!(
                "{current} ← {next}\n\nهل تريد تثبيته الآن؟ سيُعاد تشغيل {} وسيعيدك إلى حيث توقفت.",
                self.product
            ),
        }
    }

    pub fn update_install_now(&self) -> String {
        self.pick("Install now", "التثبيت الآن")
    }

    pub fn update_remind_later(&self) -> String {
        self.pick("Remind me later", "ذكّرني لاحقاً")
    }

    pub fn update_skip_version(&self) -> String {
        self.pick("Skip this version", "تخطّي هذا الإصدار")
    }

    pub fn update_notes_heading(&self) -> String {
        self.pick("What's new", "ما الجديد")
    }

    /// Shown when the release manifest carries no notes. Saying nothing at all
    /// would leave a labelled but empty panel, which reads as broken.
    pub fn update_notes_fallback(&self) -> String {
        self.pick(
            "Improvements and fixes across the app.",
            "تحسينات وإصلاحات في مختلف أنحاء التطبيق.",
        )
    }

    pub fn update_progress_downloading(&self) -> String {
        self.pick("Downloading the update…", "جارٍ تنزيل التحديث…")
    }

    pub fn update_progress_installing(&self) -> String {
        match self.lang {
            Lang::En => format!("Installing. {} will restart in a moment.", self.product),
            Lang::Ar => format!("جارٍ التثبيت. سيُعاد تشغيل {} بعد لحظات.", self.product),
        }
    }

    pub fn update_download_failed(&self) -> String {
        self.pick(
            "The update couldn't be downloaded. Check your internet connection and try again.",
            "تعذّر تنزيل التحديث. تحقّق من اتصالك بالإنترنت وحاول مرة أخرى.",
        )
    }

    pub fn update_downloading_title(&self) -> String {
        self.pick("Update available", "يتوفر تحديث")
    }

    pub fn update_downloading_body(&self) -> String {
        match self.lang {
            Lang::En => format!("Downloading the latest version of {}…", self.product),
            Lang::Ar => format!("جارٍ تنزيل أحدث إصدار من {}…", self.product),
        }
    }

    pub fn update_installing_title(&self) -> String {
        self.pick("Finishing update", "جارٍ إكمال التحديث")
    }

    pub fn update_installing_body(&self) -> String {
        match self.lang {
            Lang::En => format!("{} will restart to finish installing.", self.product),
            Lang::Ar => format!("سيُعاد تشغيل {} لإكمال التثبيت.", self.product),
        }
    }

    pub fn update_none_title(&self) -> String {
        self.pick("You're up to date", "أنت على أحدث إصدار")
    }

    pub fn update_none_body(&self) -> String {
        match self.lang {
            Lang::En => format!("{} is running the latest version.", self.product),
            Lang::Ar => format!("يعمل {} على أحدث إصدار.", self.product),
        }
    }

    pub fn update_failed_title(&self) -> String {
        self.pick("Couldn't check for updates", "تعذّر البحث عن تحديثات")
    }

    pub fn update_failed_body(&self) -> String {
        self.pick(
            "Check your internet connection and try again.",
            "تحقّق من اتصالك بالإنترنت وحاول مرة أخرى.",
        )
    }
}
