//! Android storage access.
//!
//! On Android 11+ an app only sees all of shared storage after the user turns
//! on "All files access" for it in system settings. On Android 10 and older
//! the classic READ_EXTERNAL_STORAGE runtime permission is enough.

use std::ffi::{CString, c_char, c_int};
use std::sync::OnceLock;

use jni::objects::{JObject, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use slint::android::AndroidApp;

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

fn open_all_files_settings(env: &mut Env, activity: &JObject) -> JniResult<()> {
    let package = env
        .call_method(
            activity,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )?
        .l()?;
    let scheme = env.new_string("package")?;
    let uri = env
        .call_static_method(
            jni_str!("android/net/Uri"),
            jni_str!("fromParts"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;"),
            &[
                JValue::Object(&scheme),
                JValue::Object(&package),
                JValue::Object(&JObject::null()),
            ],
        )?
        .l()?;
    // The per-app screen is missing on some devices; fall back to the list of all apps.
    let per_app = start_activity(
        env,
        activity,
        "android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION",
        &uri,
    );
    if per_app.is_err() {
        env.exception_clear();
        start_activity(
            env,
            activity,
            "android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION",
            &JObject::null(),
        )?;
    }
    Ok(())
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
