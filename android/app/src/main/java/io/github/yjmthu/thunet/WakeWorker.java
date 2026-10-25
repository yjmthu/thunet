package io.github.yjmthu.thunet;

import android.content.Context;
import android.util.Log;
import androidx.annotation.NonNull;
import androidx.work.Worker;
import androidx.work.WorkerParameters;

/**
 * The 15-minute backstop.
 *
 * The only case nothing else covers is "the app was killed while online and a
 * Wi-Fi network appears later": the one-shot was never armed, so this job is
 * what eventually notices.
 */
public class WakeWorker extends Worker {
  public WakeWorker(@NonNull Context context, @NonNull WorkerParameters params) {
    super(context, params);
  }

  @NonNull
  @Override
  public Result doWork() {
    Log.i(Wake.TAG, "wake: worker");
    Context context = getApplicationContext();
    String mode = Wake.mode(context);
    if (Wake.MODE_WAKE.equals(mode)) {
      if (Wake.wifiAvailable(context)) {
        Native.ensureOnline(context.getFilesDir().getAbsolutePath());
      } else {
        Wake.arm(context);
      }
    }
    // The resident level is covered by its service, and "off" asked for nothing.
    return Result.success();
  }
}
