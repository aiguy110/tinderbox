//! Keep the screen awake while the forest is showing: no idle blanking,
//! locking or suspend. On the web this is a screen wake lock, which the
//! browser drops whenever the page is hidden, so it's re-requested when the
//! page comes back. On Linux it's a session inhibitor over D-Bus, held as
//! long as the connection stays open; elsewhere it does nothing (yet).

#[cfg(target_arch = "wasm32")]
mod imp {
    use wasm_bindgen::prelude::*;

    // web-sys only exposes the Wake Lock API behind an unstable cfg.
    #[wasm_bindgen(inline_js = r#"
export function keep_awake() {
  if (!("wakeLock" in navigator)) return;
  let lock = null;
  const request = () => {
    if (document.visibilityState !== "visible" || (lock && !lock.released)) return;
    navigator.wakeLock.request("screen").then((l) => { lock = l; }, () => {});
  };
  document.addEventListener("visibilitychange", request);
  // Some browsers only grant it after a tap.
  document.addEventListener("pointerdown", request);
  request();
}
"#)]
    extern "C" {
        fn keep_awake();
    }

    pub struct Awake;

    pub fn keep() -> Option<Awake> {
        keep_awake();
        Some(Awake)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// The connection the inhibitor belongs to; dropping it lifts it.
    pub struct Awake(#[allow(dead_code)] zbus::blocking::Connection);

    pub fn keep() -> Option<Awake> {
        let conn = zbus::blocking::Connection::session().ok()?;
        let reason = "Showing the forest";
        // GNOME (Ubuntu): flags 4 | 8 inhibit suspend and idle (blanking
        // and locking). Other desktops implement the freedesktop interface.
        let gnome = conn.call_method(
            Some("org.gnome.SessionManager"),
            "/org/gnome/SessionManager",
            Some("org.gnome.SessionManager"),
            "Inhibit",
            &("tinderbox", 0u32, reason, 4u32 | 8u32),
        );
        let ok = gnome.is_ok()
            || conn
                .call_method(
                    Some("org.freedesktop.ScreenSaver"),
                    "/org/freedesktop/ScreenSaver",
                    Some("org.freedesktop.ScreenSaver"),
                    "Inhibit",
                    &("tinderbox", reason),
                )
                .is_ok();
        ok.then_some(Awake(conn))
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "linux")))]
mod imp {
    pub struct Awake;

    pub fn keep() -> Option<Awake> {
        None
    }
}

pub use imp::keep;
