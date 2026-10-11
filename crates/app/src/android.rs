//! Android platform glue, through JNI.
//!
//! - Storage access: on Android 11+ an app only sees all of shared storage
//!   after the user turns on "All files access" for it. On Android 10 and
//!   older the classic READ_EXTERNAL_STORAGE runtime permission is enough.
//! - App storage: other apps' private data can't be read as files, but
//!   StorageStatsManager reports its size once the user grants "Usage access".

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;

use jni::objects::{JObject, JString, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use slint::android::AndroidApp;

use crate::apps::{AppReport, AppUsage};

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
        f(env, &activity)
    })
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

/// Storage used by every installed app, plus Android's own totals. Slow
/// (one system call per app), so call it off the UI thread.
pub fn collect_app_report() -> Result<AppReport, String> {
    let result = with_activity(|env, activity| {
        let stats = system_service(env, activity, "storagestats")?;
        let packages = env
            .call_method(
                activity,
                jni_str!("getPackageManager"),
                jni_sig!("()Landroid/content/pm/PackageManager;"),
                &[],
            )?
            .l()?;
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
        let advertised_total = env
            .call_method(
                &stats,
                jni_str!("getTotalBytes"),
                jni_sig!("(Ljava/util/UUID;)J"),
                &[JValue::Object(&internal)],
            )
            .and_then(|v| v.j());
        let advertised_total = match advertised_total {
            Ok(bytes) => u64::try_from(bytes).ok(),
            Err(_) => {
                env.exception_clear();
                None
            }
        };

        // Shared storage minus what belongs to apps (Android/data, obb, media).
        let shared = env
            .call_method(
                &stats,
                jni_str!("queryExternalStatsForUser"),
                jni_sig!("(Ljava/util/UUID;Landroid/os/UserHandle;)Landroid/app/usage/ExternalStorageStats;"),
                &[JValue::Object(&internal), JValue::Object(&user)],
            )
            .and_then(|v| v.l());
        let shared_files = match shared {
            Ok(shared) => {
                let total = long_method(env, &shared, jni_str!("getTotalBytes"))?;
                let apps = long_method(env, &shared, jni_str!("getAppBytes"))?;
                Some(total.saturating_sub(apps))
            }
            Err(_) => {
                env.exception_clear();
                None
            }
        };

        let installed = env
            .call_method(
                &packages,
                jni_str!("getInstalledApplications"),
                jni_sig!("(I)Ljava/util/List;"),
                &[JValue::Int(0)],
            )?
            .l()?;
        let count = env
            .call_method(&installed, jni_str!("size"), jni_sig!("()I"), &[])?
            .i()?;

        let mut apps = Vec::new();
        for index in 0..count {
            // A frame per app keeps the number of live local references small.
            let app = env.with_local_frame(16, |env| -> JniResult<Option<AppUsage>> {
                app_usage(env, &installed, index, &stats, &packages, &user)
            });
            match app {
                Ok(Some(app)) => apps.push(app),
                Ok(None) => {}
                Err(err) => {
                    env.exception_clear();
                    log(&format!("skipping app {index}: {err}"));
                }
            }
        }
        Ok(AppReport {
            apps,
            shared_files,
            advertised_total,
        })
    });
    result.map_err(|err: jni::errors::Error| err.to_string())
}

fn app_usage(
    env: &mut Env,
    installed: &JObject,
    index: i32,
    stats: &JObject,
    packages: &JObject,
    user: &JObject,
) -> JniResult<Option<AppUsage>> {
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
    let flags = env
        .get_field(&info, jni_str!("flags"), jni_sig!("I"))?
        .i()?;
    let volume = env
        .get_field(&info, jni_str!("storageUuid"), jni_sig!("Ljava/util/UUID;"))?
        .l()?;

    // Fails for apps uninstalled since the list was made; skip those.
    let usage = env.call_method(
        stats,
        jni_str!("queryStatsForPackage"),
        jni_sig!("(Ljava/util/UUID;Ljava/lang/String;Landroid/os/UserHandle;)Landroid/app/usage/StorageStats;"),
        &[JValue::Object(&volume), JValue::Object(&package), JValue::Object(user)],
    );
    let usage = match usage.and_then(|v| v.l()) {
        Ok(usage) => usage,
        Err(_) => {
            env.exception_clear();
            return Ok(None);
        }
    };
    let code_bytes = long_method(env, &usage, jni_str!("getAppBytes"))?;
    let data_bytes = long_method(env, &usage, jni_str!("getDataBytes"))?;
    let cache_bytes = long_method(env, &usage, jni_str!("getCacheBytes"))?;

    let label = env
        .call_method(
            packages,
            jni_str!("getApplicationLabel"),
            jni_sig!("(Landroid/content/pm/ApplicationInfo;)Ljava/lang/CharSequence;"),
            &[JValue::Object(&info)],
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
    let label = env.cast_local::<JString>(label)?.try_to_string(env)?;

    Ok(Some(AppUsage {
        package: package.try_to_string(env)?,
        label,
        code_bytes,
        data_bytes,
        cache_bytes,
        system_image: flags & FLAG_SYSTEM != 0 && flags & FLAG_UPDATED_SYSTEM_APP == 0,
    }))
}
