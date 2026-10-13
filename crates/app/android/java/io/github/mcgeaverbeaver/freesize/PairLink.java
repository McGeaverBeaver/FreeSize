package io.github.mcgeaverbeaver.freesize;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.util.Log;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * Receives a scanned pairing link (freesize://pair?..., or disktree://pair?...
 * from a Hub not updated since the rename) and hands it to the app.
 *
 * <p>The app's own activity is a NativeActivity in singleTask mode: when it's
 * already running, Android gives it the new link through onNewIntent, which
 * NativeActivity ignores, so the app came to the front without the link.
 * This activity, which shows nothing, writes the link to {@code pair-link} in
 * the app's files folder and opens the app, which reads and removes the file
 * on start and while it runs, and opens the Hub's connect sheet with it.
 */
public final class PairLink extends Activity {
    /** Read by the Rust side (android::take_pair_link). */
    public static final String FILE = "pair-link";

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Uri data = getIntent() != null ? getIntent().getData() : null;
        if (data != null) {
            File file = new File(getFilesDir(), FILE);
            try (FileOutputStream out = new FileOutputStream(file)) {
                out.write(data.toString().getBytes(StandardCharsets.UTF_8));
            } catch (Exception error) {
                Log.w("FreeSize", "couldn't keep the pairing link", error);
            }
        }
        Intent open = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (open != null) {
            open.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(open);
        }
        // Theme.NoDisplay: must finish before onResume.
        finish();
    }
}
