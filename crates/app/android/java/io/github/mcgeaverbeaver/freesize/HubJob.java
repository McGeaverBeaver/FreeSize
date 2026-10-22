package io.github.mcgeaverbeaver.freesize;

import android.app.ActivityManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.util.Log;
import org.json.JSONArray;
import org.json.JSONObject;

/**
 * The phone in the background (0.14): every 15 minutes Android runs this
 * job, which calls into the Rust side ({@code nativeRun} in
 * libfreesize_app.so) to send what the app couldn't while it was closed and
 * to ask the Hub for news. Anything that needs the person, a request for
 * this phone or an approval for another device, comes back as a notification
 * that opens the app, where it's shown with every path and approved or
 * denied as always. Nothing runs from here; the logic stays in Rust.
 *
 * <p>Scheduled by the app (through JNI) once it's paired with a Hub, with
 * {@link #schedule}; it survives reboots. The job is periodic and needs a
 * network, so Android fits it into its own batches and the battery screen
 * never lists FreeSize above the usual apps.
 */
public final class HubJob extends JobService {
    private static final String TAG = "FreeSize";
    private static final int JOB_ID = 1;
    /** Android's shortest period for a periodic job. */
    private static final long EVERY_MS = 15L * 60 * 1000;
    private static final String CHANNEL = "hub";
    /** Set on the launch intent when a notification opened the app. */
    public static final String EXTRA_REQUEST = "io.github.mcgeaverbeaver.freesize.request";

    static {
        System.loadLibrary("freesize_app");
    }

    /**
     * Runs the background round in Rust and answers with what to notify, as
     * JSON: {@code {"notices": [{"tag": "...", "title": "...", "text": "..."}],
     * "cancel": ["tag", ...]}}, or an empty string when there's nothing to do.
     */
    private static native String nativeRun(Context context, String dataDir);

    /** Schedules the job, unless it's already pending. */
    public static void schedule(Context context) {
        JobScheduler scheduler = (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler == null) {
            return;
        }
        if (scheduler.getPendingJob(JOB_ID) != null) {
            return;
        }
        JobInfo job = new JobInfo.Builder(JOB_ID, new ComponentName(context, HubJob.class))
                .setPeriodic(EVERY_MS)
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                .setPersisted(true)
                .build();
        int result = scheduler.schedule(job);
        Log.i(TAG, result == JobScheduler.RESULT_SUCCESS ? "Hub job scheduled" : "Hub job not scheduled: " + result);
    }

    /** Stops the job, after disconnecting from the Hub. */
    public static void cancel(Context context) {
        JobScheduler scheduler = (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler != null) {
            scheduler.cancel(JOB_ID);
        }
    }

    /** Whether notifications may be shown: Android 13+ asks the person first. */
    public static boolean mayNotify(Context context) {
        if (Build.VERSION.SDK_INT < 33) {
            return true;
        }
        return context.checkSelfPermission("android.permission.POST_NOTIFICATIONS") == PackageManager.PERMISSION_GRANTED;
    }

    @Override
    public boolean onStartJob(final JobParameters params) {
        final Context context = getApplicationContext();
        Thread worker = new Thread(() -> {
            try {
                String answer = nativeRun(context, context.getFilesDir().getAbsolutePath());
                if (answer != null && !answer.isEmpty()) {
                    notify(context, answer);
                }
            } catch (Throwable error) {
                Log.w(TAG, "Hub job failed", error);
            } finally {
                jobFinished(params, false);
            }
        }, "freesize-hub-job");
        worker.setDaemon(true);
        worker.start();
        return true;
    }

    @Override
    public boolean onStopJob(JobParameters params) {
        // Android took the slot back (the network went, or the phone is
        // busy): run again at the next period.
        return true;
    }

    /**
     * The same notices from the running app, which polls the Hub itself
     * then: shown only while FreeSize isn't on screen, where the request
     * opens as a dialog anyway. Taking notices down always happens.
     */
    public static void fromApp(Context context, String json) {
        try {
            ActivityManager.RunningAppProcessInfo me = new ActivityManager.RunningAppProcessInfo();
            ActivityManager.getMyMemoryState(me);
            JSONObject answer = new JSONObject(json);
            if (me.importance <= ActivityManager.RunningAppProcessInfo.IMPORTANCE_FOREGROUND) {
                answer.remove("notices");
            }
            notify(context, answer.toString());
        } catch (Throwable error) {
            Log.w(TAG, "Hub notices failed", error);
        }
    }

    private static void notify(Context context, String json) throws Exception {
        NotificationManager manager = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager == null) {
            return;
        }
        JSONObject answer = new JSONObject(json);
        JSONArray cancel = answer.optJSONArray("cancel");
        for (int i = 0; cancel != null && i < cancel.length(); i++) {
            manager.cancel(cancel.getString(i), 0);
        }
        JSONArray notices = answer.optJSONArray("notices");
        if (notices == null || notices.length() == 0) {
            return;
        }
        if (!mayNotify(context)) {
            Log.i(TAG, "notifications aren't allowed; " + notices.length() + " waiting in the app");
            return;
        }
        NotificationChannel channel = new NotificationChannel(CHANNEL, "Requests from the Hub", NotificationManager.IMPORTANCE_HIGH);
        channel.setDescription("Requests to delete or clean up on this phone, and approvals for other devices. Nothing happens until you open FreeSize and allow it.");
        manager.createNotificationChannel(channel);
        int icon = context.getResources().getIdentifier("ic_launcher_monochrome", "mipmap", context.getPackageName());
        if (icon == 0) {
            icon = android.R.drawable.stat_notify_more;
        }
        for (int i = 0; i < notices.length(); i++) {
            JSONObject notice = notices.getJSONObject(i);
            String tag = notice.getString("tag");
            Intent open = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
            if (open == null) {
                return;
            }
            open.putExtra(EXTRA_REQUEST, tag);
            open.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            PendingIntent tap = PendingIntent.getActivity(context, tag.hashCode(), open,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
            Notification notification = new Notification.Builder(context, CHANNEL)
                    .setSmallIcon(icon)
                    .setContentTitle(notice.getString("title"))
                    .setContentText(notice.getString("text"))
                    .setStyle(new Notification.BigTextStyle().bigText(notice.getString("text")))
                    .setContentIntent(tap)
                    .setAutoCancel(true)
                    .setOnlyAlertOnce(true)
                    .setCategory(Notification.CATEGORY_REMINDER)
                    .build();
            manager.notify(tag, 0, notification);
        }
    }
}
