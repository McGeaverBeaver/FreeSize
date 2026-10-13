//! Android platform glue, through JNI.
//!
//! - Storage access: on Android 11+ an app only sees all of shared storage
//!   after the user turns on "All files access" for it. On Android 10 and
//!   older the classic READ_EXTERNAL_STORAGE runtime permission is enough.
//! - App storage: other apps' private data can't be read as files, but
//!   StorageStatsManager reports its size once the user grants "Usage access".
//!   The same permission lets UsageStatsManager say when each app was last
//!   used.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{JObject, JObjectArray, JString, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use slint::android::AndroidApp;

use crate::apps::{self, Abilities, AppReport, AppUsage};
use crate::cleanup::{self, Junk, JunkKind};
use crate::owners::Pixels;

type JniResult<T> = jni::errors::Result<T>;

static APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn init(app: AndroidApp) {
    let _ = APP.set(app);
}

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

const LOG_WARN: c_int = 5;
const LOG_ERROR: c_int = 6;

fn write_log(priority: c_int, message: &str) {
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        __android_log_write(priority, c"DiskTree".as_ptr(), text.as_ptr());
    }
}

/// Logs a warning to logcat (`adb logcat -s DiskTree`); stderr goes nowhere on Android.
pub fn log(message: &str) {
    write_log(LOG_WARN, message);
}

/// Sends panic messages to logcat so a crash leaves a readable reason behind.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_log(LOG_ERROR, &format!("panic: {info}"));
        default(info);
    }));
}

fn with_activity<T>(f: impl FnOnce(&mut Env, &JObject) -> JniResult<T>) -> JniResult<T> {
    let app = APP.get().expect("android::init not called");
    // SAFETY: android-activity guarantees a valid JavaVM pointer for the
    // lifetime of the process.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: android-activity holds a global reference to the activity
        // for the lifetime of the app; JObject never deletes it.
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        let result = f(env, &activity);
        // A failed call leaves its Java exception pending, which would make
        // the next JNI call on this thread fail too.
        if result.is_err() {
            env.exception_clear();
        }
        result
    })
}

/// Runs a lookup that is allowed to fail, clearing any exception it leaves.
fn optional<'local, T>(
    env: &mut Env<'local>,
    f: impl FnOnce(&mut Env<'local>) -> JniResult<T>,
) -> Option<T> {
    match f(env) {
        Ok(value) => Some(value),
        Err(_) => {
            env.exception_clear();
            None
        }
    }
}

fn java_string(env: &mut Env, obj: JObject) -> JniResult<String> {
    env.cast_local::<JString>(obj)?.try_to_string(env)
}

/// A Java string that may be null.
fn java_string_opt(env: &mut Env, obj: JObject) -> JniResult<Option<String>> {
    if obj.is_null() {
        Ok(None)
    } else {
        java_string(env, obj).map(Some)
    }
}

/// Milliseconds since 1970 to seconds; Java uses 0 for "never".
fn secs(ms: i64) -> Option<u64> {
    u64::try_from(ms)
        .ok()
        .filter(|&ms| ms > 0)
        .map(|ms| ms / 1000)
}

static SDK: OnceLock<i32> = OnceLock::new();

/// The Android version's API level, e.g. 35 for Android 15.
fn sdk() -> i32 {
    *SDK.get_or_init(|| with_activity(|env, _| sdk_int(env)).unwrap_or(0))
}

fn sdk_int(env: &mut Env) -> JniResult<i32> {
    env.get_static_field(
        jni_str!("android/os/Build$VERSION"),
        jni_str!("SDK_INT"),
        jni_sig!("I"),
    )?
    .i()
}

/// Whether the app may read everything in shared storage.
pub fn has_storage_access() -> bool {
    let result = with_activity(|env, activity| {
        if sdk_int(env)? >= 30 {
            env.call_static_method(
                jni_str!("android/os/Environment"),
                jni_str!("isExternalStorageManager"),
                jni_sig!("()Z"),
                &[],
            )?
            .z()
        } else {
            let permission = env.new_string("android.permission.READ_EXTERNAL_STORAGE")?;
            let state = env
                .call_method(
                    activity,
                    jni_str!("checkSelfPermission"),
                    jni_sig!("(Ljava/lang/String;)I"),
                    &[JValue::Object(&permission)],
                )?
                .i()?;
            // PackageManager.PERMISSION_GRANTED
            Ok(state == 0)
        }
    });
    result.unwrap_or(false)
}

/// Shows the system screen (or dialog) where the user grants storage access.
pub fn request_storage_access() {
    let result = with_activity(|env, activity| {
        if sdk_int(env)? >= 30 {
            open_all_files_settings(env, activity)
        } else {
            request_legacy_permissions(env, activity)
        }
    });
    if let Err(err) = result {
        log(&format!("could not request storage access: {err}"));
    }
}

fn start_activity(env: &mut Env, activity: &JObject, action: &str, uri: &JObject) -> JniResult<()> {
    let action = env.new_string(action)?;
    let intent = if uri.is_null() {
        env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?
    } else {
        env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
            &[JValue::Object(&action), JValue::Object(uri)],
        )?
    };
    env.call_method(
        activity,
        jni_str!("startActivity"),
        jni_sig!("(Landroid/content/Intent;)V"),
        &[JValue::Object(&intent)],
    )?;
    Ok(())
}

fn own_package<'local>(env: &mut Env<'local>, activity: &JObject) -> JniResult<JObject<'local>> {
    env.call_method(
        activity,
        jni_str!("getPackageName"),
        jni_sig!("()Ljava/lang/String;"),
        &[],
    )?
    .l()
}

/// `package:<name>`, the data URI settings screens use to pick an app.
fn package_uri<'local>(env: &mut Env<'local>, package: &JObject) -> JniResult<JObject<'local>> {
    let scheme = env.new_string("package")?;
    env.call_static_method(
        jni_str!("android/net/Uri"),
        jni_str!("fromParts"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;"),
        &[
            JValue::Object(&scheme),
            JValue::Object(package),
            JValue::Object(&JObject::null()),
        ],
    )?
    .l()
}

/// Opens a settings screen for this app, or the general one if the device
/// has no per-app version.
fn open_settings(env: &mut Env, activity: &JObject, per_app: &str, general: &str) -> JniResult<()> {
    let package = own_package(env, activity)?;
    let uri = package_uri(env, &package)?;
    if start_activity(env, activity, per_app, &uri).is_err() {
        env.exception_clear();
        start_activity(env, activity, general, &JObject::null())?;
    }
    Ok(())
}

fn open_all_files_settings(env: &mut Env, activity: &JObject) -> JniResult<()> {
    open_settings(
        env,
        activity,
        "android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION",
        "android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION",
    )
}

fn request_legacy_permissions(env: &mut Env, activity: &JObject) -> JniResult<()> {
    let names = [
        "android.permission.READ_EXTERNAL_STORAGE",
        "android.permission.WRITE_EXTERNAL_STORAGE",
    ];
    let array = env.new_object_array(
        names.len() as i32,
        jni_str!("java/lang/String"),
        JObject::null(),
    )?;
    for (i, name) in names.iter().enumerate() {
        let name = env.new_string(name)?;
        array.set_element(env, i, &name)?;
    }
    env.call_method(
        activity,
        jni_str!("requestPermissions"),
        jni_sig!("([Ljava/lang/String;I)V"),
        &[JValue::Object(&array), JValue::Int(1)],
    )?;
    Ok(())
}

fn system_service<'local>(
    env: &mut Env<'local>,
    activity: &JObject,
    name: &str,
) -> JniResult<JObject<'local>> {
    let name = env.new_string(name)?;
    env.call_method(
        activity,
        jni_str!("getSystemService"),
        jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
        &[JValue::Object(&name)],
    )?
    .l()
}

fn long_method(
    env: &mut Env,
    obj: &JObject,
    name: &'static jni::strings::JNIStr,
) -> JniResult<u64> {
    let value = env.call_method(obj, name, jni_sig!("()J"), &[])?.j()?;
    Ok(u64::try_from(value).unwrap_or(0))
}

/// Whether the user granted "Usage access", which StorageStatsManager needs
/// to report other apps' storage.
pub fn has_usage_access() -> bool {
    let result = with_activity(|env, activity| {
        let app_ops = system_service(env, activity, "appops")?;
        let op = env.new_string("android:get_usage_stats")?;
        let uid = env
            .call_static_method(
                jni_str!("android/os/Process"),
                jni_str!("myUid"),
                jni_sig!("()I"),
                &[],
            )?
            .i()?;
        let package = own_package(env, activity)?;
        let method = if sdk_int(env)? >= 29 {
            jni_str!("unsafeCheckOpNoThrow")
        } else {
            jni_str!("checkOpNoThrow")
        };
        let mode = env
            .call_method(
                &app_ops,
                method,
                jni_sig!("(Ljava/lang/String;ILjava/lang/String;)I"),
                &[
                    JValue::Object(&op),
                    JValue::Int(uid),
                    JValue::Object(&package),
                ],
            )?
            .i()?;
        // AppOpsManager.MODE_ALLOWED
        Ok(mode == 0)
    });
    result.unwrap_or(false)
}

/// Opens the "Usage access" settings screen.
pub fn request_usage_access() {
    let result = with_activity(|env, activity| {
        open_settings(
            env,
            activity,
            "android.settings.USAGE_ACCESS_SETTINGS",
            "android.settings.USAGE_ACCESS_SETTINGS",
        )
    });
    if let Err(err) = result {
        log(&format!("could not open usage access settings: {err}"));
    }
}

/// Opens an app's page in system settings, where the user can clear its
/// cache or data, or uninstall it.
pub fn open_app_settings(package: &str) {
    let result = with_activity(|env, activity| {
        let package = env.new_string(package)?;
        let uri = package_uri(env, &package)?;
        start_activity(
            env,
            activity,
            "android.settings.APPLICATION_DETAILS_SETTINGS",
            &uri,
        )
    });
    if let Err(err) = result {
        log(&format!("could not open app settings: {err}"));
    }
}

/// Opens DiskTree's own page in system settings. Its ⋮ menu holds "Allow
/// restricted settings", which Android requires before granting Usage
/// access to an app installed from a downloaded APK.
pub fn open_own_app_settings() {
    let result = with_activity(|env, activity| {
        let package = own_package(env, activity)?;
        let uri = package_uri(env, &package)?;
        start_activity(
            env,
            activity,
            "android.settings.APPLICATION_DETAILS_SETTINGS",
            &uri,
        )
    });
    if let Err(err) = result {
        log(&format!("could not open DiskTree's app settings: {err}"));
    }
}

/// What DiskTree can do with apps on this device.
pub fn abilities() -> Abilities {
    let sdk = sdk();
    Abilities {
        uninstall: true,
        // The Archive button on an app's info page.
        archive: sdk >= 35,
        // StorageManager.ACTION_CLEAR_APP_CACHE.
        clear_cache: sdk >= 30,
        // StorageStats.getExternalCacheBytes.
        shared_cache_known: sdk >= 31,
        save_apk: true,
    }
}

/// DiskTree's private storage, which needs no permission.
pub fn private_dir() -> Option<PathBuf> {
    APP.get()?.internal_data_path()
}

/// Whether DiskTree's window has focus. It loses it while a system screen
/// or dialog is in front.
pub fn window_has_focus() -> bool {
    with_activity(|env, activity| {
        env.call_method(activity, jni_str!("hasWindowFocus"), jni_sig!("()Z"), &[])?
            .z()
    })
    .unwrap_or(true)
}

/// Asks Android to uninstall an app. Android shows its own confirmation.
/// Needs the REQUEST_DELETE_PACKAGES permission.
pub fn uninstall(package: &str) {
    let result = with_activity(|env, activity| {
        let package = env.new_string(package)?;
        let uri = package_uri(env, &package)?;
        start_activity(env, activity, "android.intent.action.DELETE", &uri)
    });
    if let Err(err) = result {
        log(&format!("could not start uninstalling: {err}"));
    }
}

/// Opens a URL with `ACTION_VIEW`, in the app `package` if given.
fn view_url(env: &mut Env, activity: &JObject, url: &str, package: Option<&str>) -> JniResult<()> {
    let url = env.new_string(url)?;
    let uri = env
        .call_static_method(
            jni_str!("android/net/Uri"),
            jni_str!("parse"),
            jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
            &[JValue::Object(&url)],
        )?
        .l()?;
    let action = env.new_string("android.intent.action.VIEW")?;
    let intent = env.new_object(
        jni_str!("android/content/Intent"),
        jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
        &[JValue::Object(&action), JValue::Object(&uri)],
    )?;
    if let Some(package) = package {
        let package = env.new_string(package)?;
        env.call_method(
            &intent,
            jni_str!("setPackage"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&package)],
        )?;
    }
    env.call_method(
        activity,
        jni_str!("startActivity"),
        jni_sig!("(Landroid/content/Intent;)V"),
        &[JValue::Object(&intent)],
    )?;
    Ok(())
}

/// Opens a `market://` link, preferring `store`, then any app that handles
/// such links, then the Play Store website.
fn open_market(market: &str, web: &str, store: Option<&str>) {
    let result = with_activity(|env, activity| {
        if let Some(store) = store
            && optional(env, |env| view_url(env, activity, market, Some(store))).is_some()
        {
            return Ok(());
        }
        if optional(env, |env| view_url(env, activity, market, None)).is_some() {
            return Ok(());
        }
        view_url(env, activity, web, None)
    });
    if let Err(err) = result {
        log(&format!("could not open the store: {err}"));
    }
}

/// Opens an app's page in the store that installed it.
pub fn open_in_store(package: &str, installer: Option<&str>) {
    open_market(
        &format!("market://details?id={package}"),
        &format!("https://play.google.com/store/apps/details?id={package}"),
        installer,
    );
}

/// Searches the Play Store for apps.
pub fn search_store(query: &str) {
    let query = apps::url_encode(query);
    open_market(
        &format!("market://search?q={query}&c=apps"),
        &format!("https://play.google.com/store/search?q={query}&c=apps"),
        Some("com.android.vending"),
    );
}

/// Shows Android's dialog that empties every app's cache folder in shared
/// storage (`Android/data/*/cache`; Android 11+, for apps with All files
/// access). It can't touch the cache in apps' private storage, usually most
/// of it. Falls back to the Storage settings screen and returns false.
pub fn clear_all_cache() -> bool {
    let result = with_activity(|env, activity| {
        let action = env.new_string("android.os.storage.action.CLEAR_APP_CACHE")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?;
        // The dialog checks the caller's permission, which it can only see
        // when started for a result.
        env.call_method(
            activity,
            jni_str!("startActivityForResult"),
            jni_sig!("(Landroid/content/Intent;I)V"),
            &[JValue::Object(&intent), JValue::Int(7)],
        )?;
        Ok(())
    });
    if let Err(err) = result {
        log(&format!("no clear-cache dialog: {err}"));
        let fallback = with_activity(|env, activity| {
            start_activity(
                env,
                activity,
                "android.settings.INTERNAL_STORAGE_SETTINGS",
                &JObject::null(),
            )
        });
        if let Err(err) = fallback {
            log(&format!("could not open storage settings: {err}"));
        }
        return false;
    }
    true
}

fn package_manager<'local>(
    env: &mut Env<'local>,
    activity: &JObject,
) -> JniResult<JObject<'local>> {
    env.call_method(
        activity,
        jni_str!("getPackageManager"),
        jni_sig!("()Landroid/content/pm/PackageManager;"),
        &[],
    )?
    .l()
}

fn application_info<'local>(
    env: &mut Env<'local>,
    packages: &JObject,
    package: &str,
) -> JniResult<JObject<'local>> {
    let name = env.new_string(package)?;
    env.call_method(
        packages,
        jni_str!("getApplicationInfo"),
        jni_sig!("(Ljava/lang/String;I)Landroid/content/pm/ApplicationInfo;"),
        &[JValue::Object(&name), JValue::Int(0)],
    )?
    .l()
}

/// The paths of an app's APK and its split APKs, base first.
pub fn apk_files(package: &str) -> Result<Vec<PathBuf>, String> {
    let result = with_activity(|env, activity| {
        let packages = package_manager(env, activity)?;
        let info = application_info(env, &packages, package)?;
        let base = env
            .get_field(&info, jni_str!("sourceDir"), jni_sig!("Ljava/lang/String;"))?
            .l()?;
        let mut files = vec![PathBuf::from(java_string(env, base)?)];
        let splits = env
            .get_field(
                &info,
                jni_str!("splitSourceDirs"),
                jni_sig!("[Ljava/lang/String;"),
            )?
            .l()?;
        if !splits.is_null() {
            let splits = env.cast_local::<JObjectArray>(splits)?;
            for i in 0..splits.len(env)? {
                let split = splits.get_element(env, i)?;
                files.push(PathBuf::from(java_string(env, split)?));
            }
        }
        Ok(files)
    });
    result.map_err(|err| format!("Couldn't find the app's install files: {err}"))
}

fn label_of(env: &mut Env, packages: &JObject, info: &JObject) -> JniResult<String> {
    let label = env
        .call_method(
            packages,
            jni_str!("getApplicationLabel"),
            jni_sig!("(Landroid/content/pm/ApplicationInfo;)Ljava/lang/CharSequence;"),
            &[JValue::Object(info)],
        )?
        .l()?;
    let label = env
        .call_method(
            &label,
            jni_str!("toString"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    java_string(env, label)
}

/// When each app was last on screen and how long it was on screen lately,
/// from Android's usage history (which Usage access allows reading).
#[derive(Default)]
struct Usage {
    /// Package to seconds since 1970.
    last_opened: HashMap<String, u64>,
    /// Package to seconds on screen over about the last month.
    screen_time: HashMap<String, u64>,
}

/// `(package, last time on screen in ms, time on screen in ms)` from every
/// record of one interval type (daily, weekly, monthly or yearly) that
/// overlaps the time range.
fn usage_stats(
    env: &mut Env,
    manager: &JObject,
    interval: i32,
    begin_ms: i64,
    end_ms: i64,
    sdk: i32,
) -> JniResult<Vec<(String, i64, i64)>> {
    let list = env
        .call_method(
            manager,
            jni_str!("queryUsageStats"),
            jni_sig!("(IJJ)Ljava/util/List;"),
            &[
                JValue::Int(interval),
                JValue::Long(begin_ms),
                JValue::Long(end_ms),
            ],
        )?
        .l()?;
    if list.is_null() {
        return Ok(Vec::new());
    }
    let count = env
        .call_method(&list, jni_str!("size"), jni_sig!("()I"), &[])?
        .i()?;
    let mut rows = Vec::new();
    for i in 0..count {
        let row = env.with_local_frame(8, |env| -> JniResult<_> {
            let stats = env
                .call_method(
                    &list,
                    jni_str!("get"),
                    jni_sig!("(I)Ljava/lang/Object;"),
                    &[JValue::Int(i)],
                )?
                .l()?;
            let package = env
                .call_method(
                    &stats,
                    jni_str!("getPackageName"),
                    jni_sig!("()Ljava/lang/String;"),
                    &[],
                )?
                .l()?;
            let package = java_string(env, package)?;
            // "Visible" (Android 10+) means on screen; "used" also counts
            // background work.
            let (last, total) = if sdk >= 29 {
                (
                    jni_str!("getLastTimeVisible"),
                    jni_str!("getTotalTimeVisible"),
                )
            } else {
                (
                    jni_str!("getLastTimeUsed"),
                    jni_str!("getTotalTimeInForeground"),
                )
            };
            let last = env.call_method(&stats, last, jni_sig!("()J"), &[])?.j()?;
            let total = env.call_method(&stats, total, jni_sig!("()J"), &[])?.j()?;
            Ok((package, last, total))
        })?;
        rows.push(row);
    }
    Ok(rows)
}

/// Reads the usage history. Each interval type is asked for on its own:
/// asked for two years at once, Android falls back to its daily records
/// when none of its records start that far back, and those only cover
/// about a week.
fn usage_history(env: &mut Env, activity: &JObject, now_ms: i64, sdk: i32) -> JniResult<Usage> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    // UsageStatsManager.INTERVAL_DAILY, _WEEKLY, _MONTHLY, _YEARLY
    const DAILY: i32 = 0;
    const WEEKLY: i32 = 1;
    const MONTHLY: i32 = 2;
    const YEARLY: i32 = 3;
    let manager = system_service(env, activity, "usagestats")?;
    let mut usage = Usage::default();
    // Android keeps about a week of daily records, four weekly, six monthly
    // and two yearly ones; together they reach two years back.
    for interval in [YEARLY, MONTHLY, WEEKLY, DAILY] {
        for (package, last_ms, _) in usage_stats(
            env,
            &manager,
            interval,
            now_ms - 2 * 365 * DAY_MS,
            now_ms,
            sdk,
        )? {
            if let Some(last) = secs(last_ms) {
                let entry = usage.last_opened.entry(package).or_default();
                *entry = (*entry).max(last);
            }
        }
    }
    // The weekly records of the last four weeks give time on screen over
    // about a month.
    for (package, _, total_ms) in
        usage_stats(env, &manager, WEEKLY, now_ms - 28 * DAY_MS, now_ms, sdk)?
    {
        *usage.screen_time.entry(package).or_default() +=
            u64::try_from(total_ms / 1000).unwrap_or(0);
    }
    Ok(usage)
}

/// The package of the app that handles an intent by default, if one is set.
fn default_handler(
    env: &mut Env,
    packages: &JObject,
    action: &str,
    category: &str,
    url: Option<&str>,
) -> JniResult<Option<String>> {
    const MATCH_DEFAULT_ONLY: i32 = 0x0001_0000;
    let action = env.new_string(action)?;
    let intent = match url {
        Some(url) => {
            let url = env.new_string(url)?;
            let uri = env
                .call_static_method(
                    jni_str!("android/net/Uri"),
                    jni_str!("parse"),
                    jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                    &[JValue::Object(&url)],
                )?
                .l()?;
            env.new_object(
                jni_str!("android/content/Intent"),
                jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
                &[JValue::Object(&action), JValue::Object(&uri)],
            )?
        }
        None => env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?,
    };
    let category = env.new_string(category)?;
    env.call_method(
        &intent,
        jni_str!("addCategory"),
        jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
        &[JValue::Object(&category)],
    )?;
    let resolved = env
        .call_method(
            packages,
            jni_str!("resolveActivity"),
            jni_sig!("(Landroid/content/Intent;I)Landroid/content/pm/ResolveInfo;"),
            &[JValue::Object(&intent), JValue::Int(MATCH_DEFAULT_ONLY)],
        )?
        .l()?;
    if resolved.is_null() {
        return Ok(None);
    }
    let activity_info = env
        .get_field(
            &resolved,
            jni_str!("activityInfo"),
            jni_sig!("Landroid/content/pm/ActivityInfo;"),
        )?
        .l()?;
    if activity_info.is_null() {
        return Ok(None);
    }
    let package = env
        .get_field(
            &activity_info,
            jni_str!("packageName"),
            jni_sig!("Ljava/lang/String;"),
        )?
        .l()?;
    let package = java_string(env, package)?;
    // "android" is the chooser, shown when there is no default.
    Ok((package != "android").then_some(package))
}

fn secure_setting(env: &mut Env, activity: &JObject, name: &str) -> JniResult<Option<String>> {
    let resolver = env
        .call_method(
            activity,
            jni_str!("getContentResolver"),
            jni_sig!("()Landroid/content/ContentResolver;"),
            &[],
        )?
        .l()?;
    let name = env.new_string(name)?;
    let value = env
        .call_static_method(
            jni_str!("android/provider/Settings$Secure"),
            jni_str!("getString"),
            jni_sig!("(Landroid/content/ContentResolver;Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(&resolver), JValue::Object(&name)],
        )?
        .l()?;
    java_string_opt(env, value)
}

fn device_admins(env: &mut Env, activity: &JObject) -> JniResult<Vec<String>> {
    let manager = system_service(env, activity, "device_policy")?;
    let admins = env
        .call_method(
            &manager,
            jni_str!("getActiveAdmins"),
            jni_sig!("()Ljava/util/List;"),
            &[],
        )?
        .l()?;
    let mut packages = Vec::new();
    if admins.is_null() {
        return Ok(packages);
    }
    let count = env
        .call_method(&admins, jni_str!("size"), jni_sig!("()I"), &[])?
        .i()?;
    for i in 0..count {
        let component = env
            .call_method(
                &admins,
                jni_str!("get"),
                jni_sig!("(I)Ljava/lang/Object;"),
                &[JValue::Int(i)],
            )?
            .l()?;
        let package = env
            .call_method(
                &component,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        packages.push(java_string(env, package)?);
    }
    Ok(packages)
}

/// Apps that do their job without being opened, and why: default apps,
/// keyboards, accessibility services, notification readers (watch apps)
/// and device admins. Each lookup may fail on its own.
fn protected_packages(
    env: &mut Env,
    activity: &JObject,
    packages: &JObject,
) -> HashMap<String, &'static str> {
    let mut found = HashMap::new();
    let defaults = [
        (
            "android.intent.action.MAIN",
            "android.intent.category.HOME",
            None,
            "your home screen app",
        ),
        (
            "android.intent.action.VIEW",
            "android.intent.category.BROWSABLE",
            Some("https://example.com"),
            "your browser",
        ),
    ];
    for (action, category, url, reason) in defaults {
        if let Some(Some(package)) = optional(env, |env| {
            default_handler(env, packages, action, category, url)
        }) {
            found.insert(package, reason);
        }
    }
    let sms = optional(env, |env| {
        let package = env
            .call_static_method(
                jni_str!("android/provider/Telephony$Sms"),
                jni_str!("getDefaultSmsPackage"),
                jni_sig!("(Landroid/content/Context;)Ljava/lang/String;"),
                &[JValue::Object(activity)],
            )?
            .l()?;
        java_string_opt(env, package)
    });
    if let Some(Some(package)) = sms {
        found.insert(package, "your messaging app");
    }
    let dialer = optional(env, |env| {
        let telecom = system_service(env, activity, "telecom")?;
        let package = env
            .call_method(
                &telecom,
                jni_str!("getDefaultDialerPackage"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        java_string_opt(env, package)
    });
    if let Some(Some(package)) = dialer {
        found.insert(package, "your phone app");
    }
    let settings = [
        ("enabled_input_methods", "a keyboard"),
        ("enabled_accessibility_services", "an accessibility service"),
        (
            "enabled_notification_listeners",
            "reading your notifications, as watch apps do",
        ),
    ];
    for (name, reason) in settings {
        if let Some(Some(list)) = optional(env, |env| secure_setting(env, activity, name)) {
            for package in apps::component_packages(&list) {
                found.entry(package).or_insert(reason);
            }
        }
    }
    if let Some(admins) = optional(env, |env| device_admins(env, activity)) {
        for package in admins {
            found.entry(package).or_insert("a device admin app");
        }
    }
    found
}

/// Every installed app. On Android 15+ archived apps are included: their
/// data still takes space.
fn installed_applications<'local>(
    env: &mut Env<'local>,
    packages: &JObject,
    sdk: i32,
) -> JniResult<JObject<'local>> {
    if sdk >= 35 {
        const MATCH_ARCHIVED_PACKAGES: i64 = 1 << 32;
        let with_archived = optional(env, |env| {
            let flags = env
                .call_static_method(
                    jni_str!("android/content/pm/PackageManager$ApplicationInfoFlags"),
                    jni_str!("of"),
                    jni_sig!("(J)Landroid/content/pm/PackageManager$ApplicationInfoFlags;"),
                    &[JValue::Long(MATCH_ARCHIVED_PACKAGES)],
                )?
                .l()?;
            env.call_method(
                packages,
                jni_str!("getInstalledApplications"),
                jni_sig!(
                    "(Landroid/content/pm/PackageManager$ApplicationInfoFlags;)Ljava/util/List;"
                ),
                &[JValue::Object(&flags)],
            )?
            .l()
        });
        if let Some(list) = with_archived {
            return Ok(list);
        }
    }
    env.call_method(
        packages,
        jni_str!("getInstalledApplications"),
        jni_sig!("(I)Ljava/util/List;"),
        &[JValue::Int(0)],
    )?
    .l()
}

/// Queries MediaStore's Files collection, which indexes every file in
/// shared storage. Returns a Cursor.
fn files_query<'local>(
    env: &mut Env<'local>,
    activity: &JObject,
    projection: &[&str],
    selection: Option<(&str, &[&str])>,
    match_trashed: Option<i32>,
) -> JniResult<JObject<'local>> {
    let volume = env.new_string("external")?;
    let uri = env
        .call_static_method(
            jni_str!("android/provider/MediaStore$Files"),
            jni_str!("getContentUri"),
            jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
            &[JValue::Object(&volume)],
        )?
        .l()?;
    let columns = string_array(env, projection)?;
    let args = env.new_object(jni_str!("android/os/Bundle"), jni_sig!("()V"), &[])?;
    if let Some((selection, values)) = selection {
        // ContentResolver.QUERY_ARG_SQL_SELECTION
        let key = env.new_string("android:query-arg-sql-selection")?;
        let value = env.new_string(selection)?;
        env.call_method(
            &args,
            jni_str!("putString"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Object(&key), JValue::Object(&value)],
        )?;
        if !values.is_empty() {
            // ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, for the `?`s.
            let key = env.new_string("android:query-arg-sql-selection-args")?;
            let array = string_array(env, values)?;
            env.call_method(
                &args,
                jni_str!("putStringArray"),
                jni_sig!("(Ljava/lang/String;[Ljava/lang/String;)V"),
                &[JValue::Object(&key), JValue::Object(&array)],
            )?;
        }
    }
    if let Some(mode) = match_trashed {
        // MediaStore.QUERY_ARG_MATCH_TRASHED
        let key = env.new_string("android:query-arg-match-trashed")?;
        env.call_method(
            &args,
            jni_str!("putInt"),
            jni_sig!("(Ljava/lang/String;I)V"),
            &[JValue::Object(&key), JValue::Int(mode)],
        )?;
    }
    let resolver = env
        .call_method(
            activity,
            jni_str!("getContentResolver"),
            jni_sig!("()Landroid/content/ContentResolver;"),
            &[],
        )?
        .l()?;
    env.call_method(
        &resolver,
        jni_str!("query"),
        jni_sig!("(Landroid/net/Uri;[Ljava/lang/String;Landroid/os/Bundle;Landroid/os/CancellationSignal;)Landroid/database/Cursor;"),
        &[
            JValue::Object(&uri),
            JValue::Object(&columns),
            JValue::Object(&args),
            JValue::Object(&JObject::null()),
        ],
    )?
    .l()
}

fn string_array<'local>(env: &mut Env<'local>, values: &[&str]) -> JniResult<JObjectArray<'local>> {
    let array = env.new_object_array(
        values.len() as i32,
        jni_str!("java/lang/String"),
        JObject::null(),
    )?;
    for (i, value) in values.iter().enumerate() {
        let value = env.new_string(value)?;
        array.set_element(env, i, &value)?;
    }
    Ok(array)
}

fn column_index(env: &mut Env, cursor: &JObject, name: &str) -> JniResult<i32> {
    let name = env.new_string(name)?;
    env.call_method(
        cursor,
        jni_str!("getColumnIndexOrThrow"),
        jni_sig!("(Ljava/lang/String;)I"),
        &[JValue::Object(&name)],
    )?
    .i()
}

/// Reads `(text column, size, path)` from every row of a cursor, then
/// closes it. `text` is `None` to read only sizes and paths.
fn read_rows(
    env: &mut Env,
    cursor: &JObject,
    text: Option<&str>,
) -> JniResult<Vec<(Option<String>, u64, PathBuf)>> {
    let read = |env: &mut Env| -> JniResult<Vec<(Option<String>, u64, PathBuf)>> {
        let text_column = match text {
            Some(name) => Some(column_index(env, cursor, name)?),
            None => None,
        };
        let size_column = column_index(env, cursor, "_size")?;
        let path_column = column_index(env, cursor, "_data")?;
        let mut rows = Vec::new();
        while env
            .call_method(cursor, jni_str!("moveToNext"), jni_sig!("()Z"), &[])?
            .z()?
        {
            // A frame per row frees each row's strings right away.
            let row = env.with_local_frame(8, |env| -> JniResult<_> {
                let string_at = |env: &mut Env, column: i32| -> JniResult<Option<String>> {
                    let value = env
                        .call_method(
                            cursor,
                            jni_str!("getString"),
                            jni_sig!("(I)Ljava/lang/String;"),
                            &[JValue::Int(column)],
                        )?
                        .l()?;
                    java_string_opt(env, value)
                };
                let text = match text_column {
                    Some(column) => string_at(env, column)?,
                    None => None,
                };
                let path = string_at(env, path_column)?;
                let size = env
                    .call_method(
                        cursor,
                        jni_str!("getLong"),
                        jni_sig!("(I)J"),
                        &[JValue::Int(size_column)],
                    )?
                    .j()?;
                Ok((text, u64::try_from(size).unwrap_or(0), path))
            })?;
            if let (text, size, Some(path)) = row {
                rows.push((text, size, PathBuf::from(path)));
            }
        }
        Ok(rows)
    };
    let rows = read(env);
    // Close the cursor even when reading failed.
    if rows.is_err() {
        env.exception_clear();
    }
    let _ = optional(env, |env| {
        env.call_method(cursor, jni_str!("close"), jni_sig!("()V"), &[])
            .map(|_| ())
    });
    rows
}

/// `(owner package, size, path)` for every file in shared storage that
/// Android knows the saving app of (Android 10+). Needs All files access.
pub fn media_rows() -> Vec<(String, u64, PathBuf)> {
    if sdk() < 29 {
        return Vec::new();
    }
    let result = with_activity(|env, activity| {
        let cursor = files_query(
            env,
            activity,
            &["owner_package_name", "_size", "_data"],
            Some(("owner_package_name IS NOT NULL AND _size > 0", &[])),
            None,
        )?;
        if cursor.is_null() {
            return Ok(Vec::new());
        }
        read_rows(env, &cursor, Some("owner_package_name"))
    });
    match result {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|(owner, size, path)| Some((owner?, size, path)))
            .collect(),
        Err(err) => {
            log(&format!("could not read the media index: {err}"));
            Vec::new()
        }
    }
}

/// Photos, videos and other files in Android's trash (Android 11+). They
/// stay on the phone for 30 days after being deleted.
pub fn trash() -> Option<Junk> {
    // MediaStore.MATCH_ONLY
    const MATCH_ONLY: i32 = 3;
    if sdk() < 30 {
        return None;
    }
    let result = with_activity(|env, activity| {
        let cursor = files_query(env, activity, &["_size", "_data"], None, Some(MATCH_ONLY))?;
        if cursor.is_null() {
            return Ok(Vec::new());
        }
        read_rows(env, &cursor, None)
    });
    let rows = match result {
        Ok(rows) => rows,
        Err(err) => {
            log(&format!("could not read the trash: {err}"));
            return None;
        }
    };
    let mut junk = Junk {
        kind: JunkKind::Trash,
        ..Junk::default()
    };
    for (_, size, path) in rows {
        let name = path
            .file_name()
            .map(|name| cleanup::untrashed_name(&name.to_string_lossy()).to_string())
            .unwrap_or_default();
        junk.bytes += size;
        junk.names.push(name);
        junk.paths.push(path);
    }
    (!junk.paths.is_empty()).then_some(junk)
}

/// Storage used by every installed app, plus Android's own totals, when
/// each app was installed and last used, and where it came from. Slow (a
/// few system calls per app), so call it off the UI thread.
pub fn collect_app_report() -> Result<AppReport, String> {
    let result = with_activity(|env, activity| {
        let sdk = sdk_int(env)?;
        let stats = system_service(env, activity, "storagestats")?;
        let packages = package_manager(env, activity)?;
        let user = env
            .call_static_method(
                jni_str!("android/os/Process"),
                jni_str!("myUserHandle"),
                jni_sig!("()Landroid/os/UserHandle;"),
                &[],
            )?
            .l()?;
        let internal = env
            .get_static_field(
                jni_str!("android/os/storage/StorageManager"),
                jni_str!("UUID_DEFAULT"),
                jni_sig!("Ljava/util/UUID;"),
            )?
            .l()?;

        // The capacity printed on the box, e.g. 128 GB.
        let advertised_total = optional(env, |env| {
            env.call_method(
                &stats,
                jni_str!("getTotalBytes"),
                jni_sig!("(Ljava/util/UUID;)J"),
                &[JValue::Object(&internal)],
            )?
            .j()
        })
        .and_then(|bytes| u64::try_from(bytes).ok());

        // Shared storage minus what belongs to apps (Android/data, obb, media).
        let shared_files = optional(env, |env| {
            let shared = env
                .call_method(
                    &stats,
                    jni_str!("queryExternalStatsForUser"),
                    jni_sig!("(Ljava/util/UUID;Landroid/os/UserHandle;)Landroid/app/usage/ExternalStorageStats;"),
                    &[JValue::Object(&internal), JValue::Object(&user)],
                )?
                .l()?;
            let total = long_method(env, &shared, jni_str!("getTotalBytes"))?;
            let apps = long_method(env, &shared, jni_str!("getAppBytes"))?;
            Ok(total.saturating_sub(apps))
        });

        let now_ms = i64::try_from(apps::now() * 1000).unwrap_or(i64::MAX);
        let usage = optional(env, |env| usage_history(env, activity, now_ms, sdk));
        let usage_known = usage.as_ref().is_some_and(|u| !u.last_opened.is_empty());
        let protected = env
            .with_local_frame(64, |env| -> JniResult<_> {
                Ok(protected_packages(env, activity, &packages))
            })
            .unwrap_or_default();
        let own = own_package(env, activity)?;
        let own = java_string(env, own)?;

        let installed = installed_applications(env, &packages, sdk)?;
        let count = env
            .call_method(&installed, jni_str!("size"), jni_sig!("()I"), &[])?
            .i()?;
        let lookup = Lookup {
            stats: &stats,
            packages: &packages,
            user: &user,
            usage: usage.as_ref(),
            sdk,
        };
        let mut names = Vec::new();
        let mut apps = Vec::new();
        for index in 0..count {
            // A frame per app keeps the number of live local references small.
            let app = env.with_local_frame(32, |env| app_usage(env, &installed, index, &lookup));
            match app {
                Ok((name, app)) => {
                    names.push(name);
                    apps.extend(app);
                }
                Err(err) => {
                    env.exception_clear();
                    log(&format!("skipping app {index}: {err}"));
                }
            }
        }

        let mut installer_labels: HashMap<String, Option<String>> = HashMap::new();
        for app in &mut apps {
            app.own = app.package == own;
            app.protected = if app.own {
                Some("DiskTree itself".into())
            } else {
                protected.get(&app.package).map(|reason| reason.to_string())
            };
            if let Some(installer) = app.installer.clone() {
                let label = installer_labels
                    .entry(installer)
                    .or_insert_with_key(|installer| {
                        optional(env, |env| {
                            env.with_local_frame(8, |env| {
                                let info = application_info(env, &packages, installer)?;
                                label_of(env, &packages, &info)
                            })
                        })
                    });
                app.installer_label = label.clone();
            }
        }
        Ok(AppReport {
            apps,
            installed: names,
            shared_files,
            advertised_total,
            usage_known,
            ..AppReport::default()
        })
    });
    result.map_err(|err: jni::errors::Error| err.to_string())
}

/// What `app_usage` needs besides the app itself.
struct Lookup<'a, 'local> {
    stats: &'a JObject<'local>,
    packages: &'a JObject<'local>,
    user: &'a JObject<'local>,
    /// The usage history, if Android reported it.
    usage: Option<&'a Usage>,
    sdk: i32,
}

fn installer_of(
    env: &mut Env,
    packages: &JObject,
    package: &JObject,
    sdk: i32,
) -> JniResult<Option<String>> {
    let name = if sdk >= 30 {
        let source = env
            .call_method(
                packages,
                jni_str!("getInstallSourceInfo"),
                jni_sig!("(Ljava/lang/String;)Landroid/content/pm/InstallSourceInfo;"),
                &[JValue::Object(package)],
            )?
            .l()?;
        env.call_method(
            &source,
            jni_str!("getInstallingPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?
    } else {
        env.call_method(
            packages,
            jni_str!("getInstallerPackageName"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
            &[JValue::Object(package)],
        )?
        .l()?
    };
    java_string_opt(env, name)
}

/// One app's facts. Returns its package name, and `None` instead of the
/// facts if its storage couldn't be read (it was uninstalled meanwhile).
fn app_usage(
    env: &mut Env,
    installed: &JObject,
    index: i32,
    lookup: &Lookup,
) -> JniResult<(String, Option<AppUsage>)> {
    const FLAG_SYSTEM: i32 = 1;
    const FLAG_UPDATED_SYSTEM_APP: i32 = 0x80;

    let info = env
        .call_method(
            installed,
            jni_str!("get"),
            jni_sig!("(I)Ljava/lang/Object;"),
            &[JValue::Int(index)],
        )?
        .l()?;
    let package = env
        .get_field(
            &info,
            jni_str!("packageName"),
            jni_sig!("Ljava/lang/String;"),
        )?
        .l()?;
    let package = env.cast_local::<JString>(package)?;
    let name = package.try_to_string(env)?;
    let flags = env
        .get_field(&info, jni_str!("flags"), jni_sig!("I"))?
        .i()?;
    let volume = env
        .get_field(&info, jni_str!("storageUuid"), jni_sig!("Ljava/util/UUID;"))?
        .l()?;

    let Some(usage) = optional(env, |env| {
        env.call_method(
            lookup.stats,
            jni_str!("queryStatsForPackage"),
            jni_sig!("(Ljava/util/UUID;Ljava/lang/String;Landroid/os/UserHandle;)Landroid/app/usage/StorageStats;"),
            &[JValue::Object(&volume), JValue::Object(&package), JValue::Object(lookup.user)],
        )?
        .l()
    }) else {
        return Ok((name, None));
    };
    let code_bytes = long_method(env, &usage, jni_str!("getAppBytes"))?;
    let data_bytes = long_method(env, &usage, jni_str!("getDataBytes"))?;
    let cache_bytes = long_method(env, &usage, jni_str!("getCacheBytes"))?;
    let label = label_of(env, lookup.packages, &info)?;

    let sdk = lookup.sdk;
    // Android 12+: the part in shared storage, the only part Android's
    // cache cleaner can empty.
    let external_cache_bytes = if sdk >= 31 {
        optional(env, |env| {
            long_method(env, &usage, jni_str!("getExternalCacheBytes"))
        })
        .unwrap_or(0)
    } else {
        0
    };
    let category = if sdk >= 26 {
        env.get_field(&info, jni_str!("category"), jni_sig!("I"))?
            .i()?
    } else {
        -1
    };
    let archived = sdk >= 35
        && optional(env, |env| {
            env.get_field(&info, jni_str!("isArchived"), jni_sig!("Z"))?
                .z()
        })
        .unwrap_or(false);
    let has_launcher = !env
        .call_method(
            lookup.packages,
            jni_str!("getLaunchIntentForPackage"),
            jni_sig!("(Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&package)],
        )?
        .l()?
        .is_null();
    let (installed_at, version) = optional(env, |env| {
        let info = env
            .call_method(
                lookup.packages,
                jni_str!("getPackageInfo"),
                jni_sig!("(Ljava/lang/String;I)Landroid/content/pm/PackageInfo;"),
                &[JValue::Object(&package), JValue::Int(0)],
            )?
            .l()?;
        let installed = env
            .get_field(&info, jni_str!("firstInstallTime"), jni_sig!("J"))?
            .j()?;
        let version = env
            .get_field(
                &info,
                jni_str!("versionName"),
                jni_sig!("Ljava/lang/String;"),
            )?
            .l()?;
        Ok((
            secs(installed),
            java_string_opt(env, version)?.unwrap_or_default(),
        ))
    })
    .unwrap_or_default();
    let installer =
        optional(env, |env| installer_of(env, lookup.packages, &package, sdk)).flatten();
    let (last_opened, screen_time) = match lookup.usage {
        Some(usage) => (
            usage.last_opened.get(&name).copied(),
            Some(usage.screen_time.get(&name).copied().unwrap_or(0)),
        ),
        None => (None, None),
    };

    let system = flags & FLAG_SYSTEM != 0;
    Ok((
        name.clone(),
        Some(AppUsage {
            package: name,
            label,
            code_bytes,
            data_bytes,
            cache_bytes,
            external_cache_bytes,
            system_image: system && flags & FLAG_UPDATED_SYSTEM_APP == 0,
            system,
            installer,
            installed_at,
            version,
            last_opened,
            screen_time,
            has_launcher,
            archived,
            category,
            ..AppUsage::default()
        }),
    ))
}

/// What happened when asked to open a file.
pub enum Opened {
    /// An app is showing it.
    Shown,
    /// Android hasn't indexed the file yet; it was asked to, so try again
    /// in a moment.
    Indexing,
}

/// Opens a file in shared storage with the app that normally shows that kind
/// of file. Other apps can't read DiskTree's file paths, so the file is
/// handed over as its MediaStore entry with permission to read it.
pub fn open_file(path: &std::path::Path) -> Result<Opened, String> {
    let path_text = path.to_string_lossy().into_owned();
    let found = with_activity(|env, activity| {
        let cursor = files_query(
            env,
            activity,
            &["_id", "mime_type"],
            Some(("_data = ?", &[path_text.as_str()])),
            None,
        )?;
        if cursor.is_null() {
            return Ok(None);
        }
        let read = |env: &mut Env| -> JniResult<Option<(i64, Option<String>)>> {
            if !env
                .call_method(&cursor, jni_str!("moveToFirst"), jni_sig!("()Z"), &[])?
                .z()?
            {
                return Ok(None);
            }
            let id_column = column_index(env, &cursor, "_id")?;
            let mime_column = column_index(env, &cursor, "mime_type")?;
            let id = env
                .call_method(
                    &cursor,
                    jni_str!("getLong"),
                    jni_sig!("(I)J"),
                    &[JValue::Int(id_column)],
                )?
                .j()?;
            let mime = env
                .call_method(
                    &cursor,
                    jni_str!("getString"),
                    jni_sig!("(I)Ljava/lang/String;"),
                    &[JValue::Int(mime_column)],
                )?
                .l()?;
            Ok(Some((id, java_string_opt(env, mime)?)))
        };
        let row = read(env);
        if row.is_err() {
            env.exception_clear();
        }
        let _ = optional(env, |env| {
            env.call_method(&cursor, jni_str!("close"), jni_sig!("()V"), &[])
                .map(|_| ())
        });
        row
    })
    .map_err(|err| format!("couldn't look the file up: {err}"))?;

    let Some((id, mime)) = found else {
        let result = with_activity(|env, activity| {
            let paths = string_array(env, &[path_text.as_str()])?;
            env.call_static_method(
                jni_str!("android/media/MediaScannerConnection"),
                jni_str!("scanFile"),
                jni_sig!("(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V"),
                &[
                    JValue::Object(activity),
                    JValue::Object(&paths),
                    JValue::Object(&JObject::null()),
                    JValue::Object(&JObject::null()),
                ],
            )?;
            Ok(())
        });
        return match result {
            Ok(()) => Ok(Opened::Indexing),
            Err(err) => Err(format!("Android couldn't index the file: {err}")),
        };
    };

    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    with_activity(|env, activity| {
        let mime = match mime.filter(|m| !m.is_empty()) {
            Some(mime) => mime,
            None => optional(env, |env| {
                let map = env
                    .call_static_method(
                        jni_str!("android/webkit/MimeTypeMap"),
                        jni_str!("getSingleton"),
                        jni_sig!("()Landroid/webkit/MimeTypeMap;"),
                        &[],
                    )?
                    .l()?;
                let ext = env.new_string(&extension)?;
                let mime = env
                    .call_method(
                        &map,
                        jni_str!("getMimeTypeFromExtension"),
                        jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                        &[JValue::Object(&ext)],
                    )?
                    .l()?;
                java_string_opt(env, mime)
            })
            .flatten()
            .unwrap_or_else(|| "*/*".into()),
        };
        // Photos, videos and music open best from their own collection.
        let collection = if mime.starts_with("image/") {
            Some(jni_str!("android/provider/MediaStore$Images$Media"))
        } else if mime.starts_with("video/") {
            Some(jni_str!("android/provider/MediaStore$Video$Media"))
        } else if mime.starts_with("audio/") {
            Some(jni_str!("android/provider/MediaStore$Audio$Media"))
        } else {
            None
        };
        let volume = env.new_string("external")?;
        let base = env
            .call_static_method(
                collection.unwrap_or(jni_str!("android/provider/MediaStore$Files")),
                jni_str!("getContentUri"),
                jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                &[JValue::Object(&volume)],
            )?
            .l()?;
        let uri = env
            .call_static_method(
                jni_str!("android/content/ContentUris"),
                jni_str!("withAppendedId"),
                jni_sig!("(Landroid/net/Uri;J)Landroid/net/Uri;"),
                &[JValue::Object(&base), JValue::Long(id)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )?;
        let mime = env.new_string(&mime)?;
        env.call_method(
            &intent,
            jni_str!("setDataAndType"),
            jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::Object(&uri), JValue::Object(&mime)],
        )?;
        // Intent.FLAG_GRANT_READ_URI_PERMISSION
        env.call_method(
            &intent,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[JValue::Int(1)],
        )?;
        env.call_method(
            activity,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
    .map(|()| Opened::Shown)
    .map_err(|_| "No app on this phone opens this kind of file.".to_string())
}

/// Icons are drawn at this size for the map and bubble views.
const ICON_SIZE: i32 = 72;

/// Drawn icons by package, with the version they were drawn for.
type IconCache = HashMap<String, (String, Arc<Pixels>)>;

static ICONS: std::sync::Mutex<Option<IconCache>> = std::sync::Mutex::new(None);

/// The icons of `apps` (`(package, version)`), drawing only those not drawn
/// before for that version.
pub fn app_icons(apps: &[(String, String)]) -> HashMap<String, Arc<Pixels>> {
    let mut guard = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    let missing: Vec<&(String, String)> = apps
        .iter()
        .filter(|(package, version)| cache.get(package).is_none_or(|(v, _)| v != version))
        .collect();
    if !missing.is_empty() {
        let drawn = with_activity(|env, activity| {
            let pm = package_manager(env, activity)?;
            let mut drawn = Vec::new();
            for (package, version) in missing {
                // A frame per icon frees its bitmap and canvas references.
                match env.with_local_frame(16, |env| app_icon(env, &pm, package)) {
                    Ok(pixels) => drawn.push((package.clone(), version.clone(), pixels)),
                    Err(_) => env.exception_clear(),
                }
            }
            Ok(drawn)
        });
        match drawn {
            Ok(drawn) => {
                for (package, version, pixels) in drawn {
                    cache.insert(package, (version, Arc::new(pixels)));
                }
            }
            Err(err) => log(&format!("could not draw app icons: {err}")),
        }
    }
    apps.iter()
        .filter_map(|(package, _)| {
            cache
                .get(package)
                .map(|(_, pixels)| (package.clone(), pixels.clone()))
        })
        .collect()
}

/// Draws an app's icon (adaptive icons with the launcher's mask) into
/// RGBA pixels.
fn app_icon(env: &mut Env, pm: &JObject, package: &str) -> JniResult<Pixels> {
    let name = env.new_string(package)?;
    let drawable = env
        .call_method(
            pm,
            jni_str!("getApplicationIcon"),
            jni_sig!("(Ljava/lang/String;)Landroid/graphics/drawable/Drawable;"),
            &[JValue::Object(&name)],
        )?
        .l()?;
    let config = env
        .get_static_field(
            jni_str!("android/graphics/Bitmap$Config"),
            jni_str!("ARGB_8888"),
            jni_sig!("Landroid/graphics/Bitmap$Config;"),
        )?
        .l()?;
    let bitmap = env
        .call_static_method(
            jni_str!("android/graphics/Bitmap"),
            jni_str!("createBitmap"),
            jni_sig!("(IILandroid/graphics/Bitmap$Config;)Landroid/graphics/Bitmap;"),
            &[
                JValue::Int(ICON_SIZE),
                JValue::Int(ICON_SIZE),
                JValue::Object(&config),
            ],
        )?
        .l()?;
    let canvas = env.new_object(
        jni_str!("android/graphics/Canvas"),
        jni_sig!("(Landroid/graphics/Bitmap;)V"),
        &[JValue::Object(&bitmap)],
    )?;
    env.call_method(
        &drawable,
        jni_str!("setBounds"),
        jni_sig!("(IIII)V"),
        &[
            JValue::Int(0),
            JValue::Int(0),
            JValue::Int(ICON_SIZE),
            JValue::Int(ICON_SIZE),
        ],
    )?;
    env.call_method(
        &drawable,
        jni_str!("draw"),
        jni_sig!("(Landroid/graphics/Canvas;)V"),
        &[JValue::Object(&canvas)],
    )?;
    let count = (ICON_SIZE * ICON_SIZE) as usize;
    let array = jni::objects::JIntArray::new(env, count)?;
    env.call_method(
        &bitmap,
        jni_str!("getPixels"),
        jni_sig!("([IIIIIII)V"),
        &[
            JValue::Object(&array),
            JValue::Int(0),
            JValue::Int(ICON_SIZE),
            JValue::Int(0),
            JValue::Int(0),
            JValue::Int(ICON_SIZE),
            JValue::Int(ICON_SIZE),
        ],
    )?;
    let mut argb = vec![0i32; count];
    array.get_region(env, 0, &mut argb)?;
    env.call_method(&bitmap, jni_str!("recycle"), jni_sig!("()V"), &[])?;
    // getPixels gives unpremultiplied ARGB, one int per pixel.
    let rgba = argb
        .iter()
        .flat_map(|&p| {
            let p = p as u32;
            [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]
        })
        .collect();
    Ok(Pixels {
        width: ICON_SIZE as u32,
        height: ICON_SIZE as u32,
        rgba,
    })
}

/// HTTP for the Hub through Java's `HttpURLConnection`, which checks
/// certificates against Android's own store (including CAs the user
/// installed, where the app allows them) and needs no native TLS library.
pub struct JavaTransport;

impl disktree_agent::Transport for JavaTransport {
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&[u8]>,
        timeout_secs: u64,
    ) -> Result<(u16, Vec<u8>), String> {
        with_activity(
            |env, _| match http(env, method, url, headers, body, timeout_secs) {
                Ok(answer) => Ok(Ok(answer)),
                // Java's own reason, like "Cleartext HTTP traffic to … not
                // permitted" or "failed to connect to …", instead of jni's
                // "Java exception was thrown".
                Err(jni::errors::Error::JavaException) => Ok(Err(java_reason(env))),
                Err(err) => Err(err),
            },
        )
        .map_err(|err| format!("{err}"))?
    }
}

/// Takes the pending Java exception and says what it was.
fn java_reason(env: &mut Env) -> String {
    match env.exception_catch() {
        Err(jni::errors::Error::CaughtJavaException { name, msg, .. }) => {
            let name = name.rsplit('.').next().unwrap_or(&name).to_string();
            if msg.is_empty() || msg == "null" {
                name
            } else {
                format!("{msg} ({name})")
            }
        }
        _ => "Java exception was thrown".into(),
    }
}

fn http(
    env: &mut Env,
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
    timeout_secs: u64,
) -> JniResult<(u16, Vec<u8>)> {
    env.with_local_frame(32, |env| -> JniResult<(u16, Vec<u8>)> {
        let url = env.new_string(url)?;
        let url = env.new_object(
            jni_str!("java/net/URL"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&url)],
        )?;
        let conn = env
            .call_method(
                &url,
                jni_str!("openConnection"),
                jni_sig!("()Ljava/net/URLConnection;"),
                &[],
            )?
            .l()?;
        let method = env.new_string(method)?;
        env.call_method(
            &conn,
            jni_str!("setRequestMethod"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&method)],
        )?;
        let millis = i32::try_from(timeout_secs.saturating_mul(1000)).unwrap_or(i32::MAX);
        env.call_method(
            &conn,
            jni_str!("setConnectTimeout"),
            jni_sig!("(I)V"),
            &[JValue::Int(millis.min(15_000))],
        )?;
        env.call_method(
            &conn,
            jni_str!("setReadTimeout"),
            jni_sig!("(I)V"),
            &[JValue::Int(millis)],
        )?;
        env.call_method(
            &conn,
            jni_str!("setInstanceFollowRedirects"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(false)],
        )?;
        for (name, value) in headers {
            let name = env.new_string(name)?;
            let value = env.new_string(value)?;
            env.call_method(
                &conn,
                jni_str!("setRequestProperty"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
                &[JValue::Object(&name), JValue::Object(&value)],
            )?;
        }
        if let Some(body) = body {
            env.call_method(
                &conn,
                jni_str!("setDoOutput"),
                jni_sig!("(Z)V"),
                &[JValue::Bool(true)],
            )?;
            let out = env
                .call_method(
                    &conn,
                    jni_str!("getOutputStream"),
                    jni_sig!("()Ljava/io/OutputStream;"),
                    &[],
                )?
                .l()?;
            let bytes = env.byte_array_from_slice(body)?;
            env.call_method(
                &out,
                jni_str!("write"),
                jni_sig!("([B)V"),
                &[JValue::Object(&bytes)],
            )?;
            env.call_method(&out, jni_str!("close"), jni_sig!("()V"), &[])?;
        }
        let status = env
            .call_method(&conn, jni_str!("getResponseCode"), jni_sig!("()I"), &[])?
            .i()?;
        // Errors come on a separate stream, which may be missing.
        let stream = if status >= 400 {
            env.call_method(
                &conn,
                jni_str!("getErrorStream"),
                jni_sig!("()Ljava/io/InputStream;"),
                &[],
            )?
            .l()?
        } else {
            env.call_method(
                &conn,
                jni_str!("getInputStream"),
                jni_sig!("()Ljava/io/InputStream;"),
                &[],
            )?
            .l()?
        };
        let mut answer = Vec::new();
        if !stream.is_null() {
            const CHUNK: usize = 16 * 1024;
            let buffer = env.new_byte_array(CHUNK)?;
            let mut chunk = vec![0i8; CHUNK];
            loop {
                let read = env
                    .call_method(
                        &stream,
                        jni_str!("read"),
                        jni_sig!("([BII)I"),
                        &[
                            JValue::Object(&buffer),
                            JValue::Int(0),
                            JValue::Int(CHUNK as i32),
                        ],
                    )?
                    .i()?;
                if read < 0 {
                    break;
                }
                let read = read as usize;
                buffer.get_region(env, 0, &mut chunk[..read])?;
                answer.extend(chunk[..read].iter().map(|&b| b as u8));
            }
            env.call_method(&stream, jni_str!("close"), jni_sig!("()V"), &[])?;
        }
        env.call_method(&conn, jni_str!("disconnect"), jni_sig!("()V"), &[])?;
        Ok((u16::try_from(status).unwrap_or(0), answer))
    })
}

/// The link the app was opened with (a `disktree://pair` link from a
/// scanned QR code), if any.
pub fn launch_link() -> Option<String> {
    with_activity(|env, activity| {
        let intent = env
            .call_method(
                activity,
                jni_str!("getIntent"),
                jni_sig!("()Landroid/content/Intent;"),
                &[],
            )?
            .l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let data = env
            .call_method(
                &intent,
                jni_str!("getDataString"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        java_string_opt(env, data)
    })
    .ok()
    .flatten()
}

/// The phone's model, e.g. "Pixel 9 Pro", for naming it at the Hub.
pub fn device_name() -> String {
    with_activity(|env, _| {
        let model = env
            .get_static_field(
                jni_str!("android/os/Build"),
                jni_str!("MODEL"),
                jni_sig!("Ljava/lang/String;"),
            )?
            .l()?;
        java_string(env, model)
    })
    .unwrap_or_else(|_| "Android phone".into())
}

/// "Android 16".
pub fn os_name() -> String {
    with_activity(|env, _| {
        let release = env
            .get_static_field(
                jni_str!("android/os/Build$VERSION"),
                jni_str!("RELEASE"),
                jni_sig!("Ljava/lang/String;"),
            )?
            .l()?;
        java_string(env, release)
    })
    .map_or_else(|_| "Android".into(), |v| format!("Android {v}"))
}
