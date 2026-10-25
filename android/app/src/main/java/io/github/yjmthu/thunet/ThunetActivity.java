package io.github.yjmthu.thunet;

import android.Manifest;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.graphics.Rect;
import android.os.Bundle;
import android.text.InputType;
import android.util.Log;
import android.util.SparseArray;
import android.view.View;
import android.view.KeyEvent;
import android.view.ViewGroup;
import android.view.ViewStructure;
import android.view.WindowInsets;
import android.view.autofill.AutofillId;
import android.view.autofill.AutofillManager;
import android.view.autofill.AutofillValue;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.window.OnBackInvokedDispatcher;

/**
 * A {@link android.app.NativeActivity} whose only job is to report facts the
 * Rust side cannot read without JNI, plus the system clipboard and the autofill
 * form it cannot provide. Everything is written as a small file into
 * {@code files/}, next to {@code config.json}:
 *
 * <ul>
 *   <li>{@code insets.txt} — system-bar and IME insets in physical pixels. Under
 *       edge-to-edge (the default for {@code targetSdk} 36)
 *       {@code AndroidApp::content_rect()} returns the whole window, so the UI
 *       pads itself instead.</li>
 *   <li>{@code theme.txt} — the system night mode. winit reports no theme on
 *       Android either, so "follow the system" would otherwise always mean
 *       dark.</li>
 * </ul>
 *
 * The link row is maintained by {@link ThunetApp}, which outlives this Activity.
 */
public class ThunetActivity extends android.app.NativeActivity {
  /** Logcat tag shared with the rest of the Java side. */
  private static final String TAG = Wake.TAG;
  /** Request code for the notification permission. */
  private static final int NOTIFY_REQUEST = 1001;
  /** Request code for the location permission (Wi-Fi name detection). */
  private static final int LOCATION_REQUEST = 1002;

  /** Autofill manager, or null on a device without the framework. */
  private AutofillManager afm;
  /** The virtual form the egui credential fields are published through. */
  private AutofillHost autofillHost;
  /** Virtual id of the credential field with the focus, if any. */
  private int focusedAutofillId = View.NO_ID;

  @Override
  protected void onCreate(Bundle savedInstanceState) {
    getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
      int bars = WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout();
      write(
          "insets.txt",
          insets.getInsets(bars).top
              + " "
              + insets.getInsets(bars).bottom
              + " "
              + insets.getInsets(WindowInsets.Type.ime()).bottom);
      return insets;
    });
    writeTheme();
    // Back has to leave the task in the background instead of finishing this
    // Activity: android-activity may run `android_main` only once per process,
    // so a finished Activity would make the next launch stick on the splash
    // screen while the core thread is still alive.
    getOnBackInvokedDispatcher()
        .registerOnBackInvokedCallback(
            OnBackInvokedDispatcher.PRIORITY_DEFAULT, this::handleBack);
    super.onCreate(savedInstanceState);
    installAutofillHost();
    // Refresh the Wi-Fi name the Rust side reads; a permission revoked in
    // Settings leaves the detection off.
    Ssid.update(this);
    // The level is the only source of truth: the service never starts itself.
    String mode = Wake.mode(this);
    if (Wake.MODE_SERVICE.equals(mode)) {
      if (notificationsAllowed()) {
        Wake.scheduleBackstop(this);
        startKeepAlive();
      } else {
        // The resident level cannot run without its notification; keep the
        // middle level and say why instead of pretending.
        Wake.apply(this, Wake.MODE_WAKE);
        Wake.setDenied(this, true);
      }
    } else if (Wake.MODE_WAKE.equals(mode)) {
      Wake.scheduleBackstop(this);
      // Covers "the app was killed while offline, then Wi-Fi appears".
      Wake.arm(this);
    }
  }

  @Override
  protected void onResume() {
    super.onResume();
    // The notification is mandatory for a foreground service; if the user
    // revoked the permission, drop to the middle level instead of running
    // invisibly.
    if (Wake.MODE_SERVICE.equals(Wake.mode(this)) && !notificationsAllowed()) {
      Wake.apply(this, Wake.MODE_WAKE);
      Wake.setDenied(this, true);
    }
    // Same for the Wi-Fi name: without the permission there is nothing to read.
    if (Ssid.enabled(this) && !Ssid.permitted(this)) {
      Ssid.setEnabled(this, false);
    } else {
      Ssid.update(this);
    }
  }

  /**
   * Called from Rust ({@code src/background.rs}) when the settings row moves.
   *
   * The answer is written by {@link Wake} in every branch, because the window
   * reads those files instead of assuming this call succeeded.
   */
  public void setMode(String mode) {
    Log.i(Wake.TAG, "background: " + mode);
    if (!Wake.MODE_SERVICE.equals(mode)) {
      Wake.setDenied(this, false);
      Wake.apply(this, mode);
      return;
    }
    if (notificationsAllowed()) {
      Wake.setDenied(this, false);
      Wake.apply(this, Wake.MODE_SERVICE);
      startKeepAlive();
      return;
    }
    requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, NOTIFY_REQUEST);
  }

  /**
   * Called from Rust ({@code src/secure_wifi.rs}) when the SSID-detection row
   * moves.
   *
   * Enabling asks for the location permission first; the answer, not this call,
   * is what the window reads back.
   */
  public void setSecureDetection(boolean on) {
    Log.i(Wake.TAG, "ssid: " + on);
    if (!on) {
      Ssid.setEnabled(this, false);
      return;
    }
    if (Ssid.permitted(this)) {
      Ssid.setEnabled(this, true);
      return;
    }
    requestPermissions(new String[] {Manifest.permission.ACCESS_FINE_LOCATION}, LOCATION_REQUEST);
  }

  @Override
  public void onRequestPermissionsResult(int request, String[] permissions, int[] results) {
    if (request == LOCATION_REQUEST) {
      Ssid.setEnabled(this, results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED);
      return;
    }
    if (request != NOTIFY_REQUEST) {
      super.onRequestPermissionsResult(request, permissions, results);
      return;
    }
    if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
      Wake.setDenied(this, false);
      Wake.apply(this, Wake.MODE_SERVICE);
      startKeepAlive();
    } else {
      // Stay at the middle level: the wake-up path needs no notification.
      Wake.apply(this, Wake.MODE_WAKE);
      Wake.setDenied(this, true);
    }
  }

  private boolean notificationsAllowed() {
    return checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
        == PackageManager.PERMISSION_GRANTED;
  }

  private void startKeepAlive() {
    try {
      startForegroundService(new Intent(this, KeepAliveService.class));
    } catch (Throwable ignored) {
      // Some ROMs refuse the start; fall back to the level that needs no
      // notification rather than leaving a dead radio selected.
      Wake.apply(this, Wake.MODE_WAKE);
      Wake.setDenied(this, true);
    }
  }

  /** The legacy back path, used when predictive back is not in effect. */
  @Override
  public void onBackPressed() {
    handleBack();
  }

  /**
   * Back: let the window step back first, and only then leave the task.
   *
   * The window answers with `false` when it has nowhere to go — no menu, no
   * modal, and the status page is already showing — which is the one case that
   * behaves like Home. Finishing the Activity is not an option: android-activity
   * can run `android_main` only once per process, so the next launch would stay
   * on the splash screen while the core kept running.
   */
  private void handleBack() {
    if (!Native.backPressed()) {
      moveTaskToBack(true);
    }
  }

  @Override
  public void onConfigurationChanged(Configuration config) {
    super.onConfigurationChanged(config);
    // Declared in the manifest, so a light/dark switch comes here instead of
    // recreating the native activity (which would restart the event loop).
    writeTheme();
  }

  /** Copy text on behalf of Rust: egui-winit's clipboard is in-process only here. */
  public void copyText(String text) {
    android.content.ClipboardManager manager =
        getSystemService(android.content.ClipboardManager.class);
    if (manager == null) {
      return;
    }
    manager.setPrimaryClip(android.content.ClipData.newPlainText("THUnet", text));
  }

  /**
   * A credential field of the egui form gained focus.
   *
   * egui draws on a surface, so there is no native text field for the autofill
   * framework to see. The form is published as a *virtual* view structure
   * instead — the route Flutter takes for its own text fields — and this call is
   * what puts the current field in front of the service, which is what makes a
   * password manager's dropdown appear over it.
   */
  public void autofillEnter(int id, int x, int y, int w, int h, String value) {
    Log.d(TAG, "autofill enter id=" + id + " rect=" + x + "," + y + " " + w + "x" + h);
    runOnUiThread(
        () -> {
          if (afm == null || autofillHost == null) {
            return;
          }
          Rect rect = new Rect(x, y, x + w, y + h);
          autofillHost.put(id, rect, value);
          if (focusedAutofillId != View.NO_ID && focusedAutofillId != id) {
            afm.notifyViewExited(autofillHost, focusedAutofillId);
          }
          focusedAutofillId = id;
          afm.notifyViewEntered(autofillHost, id, rect);
          autofillHost.attachInput(id);
        });
  }

  /** The text of the focused field changed; the save prompt depends on this. */
  public void autofillValue(int id, String value) {
    Log.d(TAG, "autofill value id=" + id + " len=" + value.length());
    runOnUiThread(
        () -> {
          if (afm == null || autofillHost == null) {
            return;
          }
          autofillHost.put(id, null, value);
          afm.notifyValueChanged(autofillHost, id, AutofillValue.forText(value));
        });
  }

  /**
   * The field moved on screen. Only the stored bounds change: re-entering the
   * view here would start another fill request, and the service's own dropdown
   * is what moves the layout in the first place.
   */
  public void autofillBounds(int id, int x, int y, int w, int h) {
    Log.d(TAG, "autofill bounds id=" + id + " rect=" + x + "," + y);
    runOnUiThread(
        () -> {
          if (autofillHost != null) {
            autofillHost.put(id, new Rect(x, y, x + w, y + h), null);
          }
        });
  }

  /** No credential field has the focus any more. */
  public void autofillLeave() {
    Log.d(TAG, "autofill leave");
    runOnUiThread(
        () -> {
          if (afm == null || autofillHost == null || focusedAutofillId == View.NO_ID) {
            return;
          }
          afm.notifyViewExited(autofillHost, focusedAutofillId);
          focusedAutofillId = View.NO_ID;
          autofillHost.detachInput();
        });
  }

  /**
   * The form was submitted (Apply): give the service its chance to offer saving
   * the pair.
   */
  public void autofillSave() {
    runOnUiThread(
        () -> {
          if (afm == null) {
            return;
          }
          try {
            afm.commit();
          } catch (RuntimeException ignored) {
            // No autofill session: the service is off or never saw the form.
          }
        });
  }

  /** The primary clipboard text, for the egui fields' long-press paste. */
  public String clipboardText() {
    ClipboardManager manager = getSystemService(ClipboardManager.class);
    if (manager == null) {
      return "";
    }
    ClipData clip = manager.getPrimaryClip();
    if (clip == null || clip.getItemCount() == 0) {
      return "";
    }
    CharSequence text = clip.getItemAt(0).coerceToText(this);
    return text == null ? "" : text.toString();
  }

  /** A short long-press tick, called from Rust when the log menu opens. */
  public void haptic() {
    runOnUiThread(
        () ->
            getWindow()
                .getDecorView()
                .performHapticFeedback(android.view.HapticFeedbackConstants.LONG_PRESS));
  }

  /** Add the invisible host that carries the egui credential form to autofill. */
  private void installAutofillHost() {
    afm = getSystemService(AutofillManager.class);
    if (afm == null) {
      return;
    }
    autofillHost = new AutofillHost(this);
    autofillHost.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_YES);
    // Focusable because it is also the keyboard's target: the egui fields
    // have no view of their own, so this host carries their InputConnection.
    autofillHost.setFocusable(true);
    autofillHost.setFocusableInTouchMode(true);
    autofillHost.setClickable(false);
    ViewGroup decor = (ViewGroup) getWindow().getDecorView();
    decor.addView(
        autofillHost,
        new ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
  }

  /**
   * The virtual credential form, and the keyboard's bridge to egui.
   *
   * A plain {@link View} with nothing drawn: it exists so the autofill
   * framework has a node in this window to hang the two virtual fields on, and
   * so the soft keyboard has a focused view with an {@link InputConnection}.
   * Touches pass straight through to the surface below because it is not
   * clickable.
   */
  private static final class AutofillHost extends View {
    /** Virtual id of the account field; one is the account, two the password. */
    static final int USER = 1;
    static final int PASSWORD = 2;

    private final SparseArray<Rect> rects = new SparseArray<>();
    private final SparseArray<String> values = new SparseArray<>();
    /** How many input connections the IME has asked for; shown in the log. */
    private int connections = 0;
    /** The field the keyboard is attached to, or {@link View#NO_ID}. */
    private int focusedFieldId = View.NO_ID;

    AutofillHost(Context context) {
      super(context);
    }

    void put(int id, Rect rect, String value) {
      if (rect != null) {
        rects.put(id, new Rect(rect));
      }
      if (value != null) {
        values.put(id, value);
      }
    }

    /** Become the keyboard's target for virtual field `id`. */
    void attachInput(int id) {
      boolean switched = focusedFieldId != id;
      focusedFieldId = id;
      if (!hasFocus()) {
        requestFocus();
      }
      InputMethodManager imm = getContext().getSystemService(InputMethodManager.class);
      if (imm == null) {
        return;
      }
      if (switched) {
        // Rebuild the connection with this field's input type (password vs
        // email); the IME caches the EditorInfo otherwise.
        imm.restartInput(this);
      }
      imm.showSoftInput(this, 0);
    }

    /** Stop being the keyboard's target and put it away. */
    void detachInput() {
      focusedFieldId = View.NO_ID;
      clearFocus();
      InputMethodManager imm = getContext().getSystemService(InputMethodManager.class);
      if (imm != null) {
        imm.hideSoftInputFromWindow(getWindowToken(), 0);
      }
    }

    @Override
    public boolean onCheckIsTextEditor() {
      return focusedFieldId != View.NO_ID;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
      if (focusedFieldId == View.NO_ID) {
        return null;
      }
      outAttrs.inputType = inputType();
      outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_ACTION_DONE;
      String text = values.get(focusedFieldId, "");
      outAttrs.initialSelStart = text.length();
      outAttrs.initialSelEnd = text.length();
      Log.d(
          TAG,
          "ime connection #"
              + (++connections)
              + " for "
              + focusedFieldId
              + " type=0x"
              + Integer.toHexString(outAttrs.inputType));
      return new ThunetInputConnection(this);
    }

    private int inputType() {
      if (focusedFieldId == PASSWORD) {
        return InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD;
      }
      // Plain text, not the email variation: some Chinese IMEs take a different
      // commit path for e-mail fields.
      return InputType.TYPE_CLASS_TEXT;
    }

    /**
     * The keyboard's end of the bridge.
     *
     * egui has no `InputConnection` on Android, so composition and candidates
     * would be dropped. Every call here becomes an egui `ImeEvent` and is
     * applied by the focused text field.
     */
    private final class ThunetInputConnection extends BaseInputConnection {
      /**
       * The composition the keyboard last sent, kept because a keyboard may
       * end it without ever calling {@code commitText} (see
       * {@link #finishComposingText()}).
       */
      private String composing = "";

      ThunetInputConnection(View target) {
        super(target, false);
      }

      @Override
      public boolean commitText(CharSequence text, int newCursorPosition) {
        Log.d(
            TAG,
            "ime commit len=" + (text == null ? 0 : text.length()) + " pos=" + newCursorPosition);
        composing = "";
        Native.imeCommit(text == null ? "" : text.toString());
        return true;
      }

      @Override
      public boolean setComposingText(CharSequence text, int newCursorPosition) {
        Log.d(TAG, "ime preedit len=" + (text == null ? 0 : text.length()));
        composing = text == null ? "" : text.toString();
        Native.imePreedit(composing);
        return true;
      }

      @Override
      public boolean finishComposingText() {
        String pending = composing;
        composing = "";
        Log.d(TAG, "ime finish pending=" + pending.length());
        if (pending.isEmpty()) {
          Native.imePreedit("");
        } else {
          // A candidate may be committed by *finishing* the composition rather
          // than by calling `commitText`; Sogou does exactly that when an English
          // suggestion is picked (`setComposingText("red")`, then finish). An
          // editor keeps the composing text as real text in that case, so the
          // pending composition is committed here instead of being dropped.
          Native.imeCommit(pending);
        }
        return true;
      }

      @Override
      public boolean deleteSurroundingText(int beforeLength, int afterLength) {
        Log.d(TAG, "ime delete " + beforeLength + "/" + afterLength);
        if (beforeLength > 0 || afterLength > 0) {
          Native.imeDelete(beforeLength, afterLength);
        }
        return true;
      }

      @Override
      public CharSequence getTextBeforeCursor(int n, int flags) {
        String text = values.get(focusedFieldId, "");
        return n > 0 && text.length() > n ? text.substring(text.length() - n) : text;
      }

      @Override
      public CharSequence getTextAfterCursor(int n, int flags) {
        return "";
      }

      @Override
      public CharSequence getSelectedText(int flags) {
        return "";
      }

      @Override
      public boolean setSelection(int start, int end) {
        Log.d(TAG, "ime selection " + start + ".." + end);
        return true;
      }

      @Override
      public boolean sendKeyEvent(KeyEvent event) {
        int key = event.getKeyCode();
        Log.d(TAG, "ime key " + key + " action=" + event.getAction());
        // A soft keyboard may delete with a key event instead of
        // `deleteSurroundingText` (Sogou does, depending on what it last
        // committed), so the two delete keys are translated here. Only the
        // press counts: the matching release would delete twice.
        if (key == KeyEvent.KEYCODE_DEL || key == KeyEvent.KEYCODE_FORWARD_DEL) {
          if (event.getAction() == KeyEvent.ACTION_DOWN) {
            if (key == KeyEvent.KEYCODE_DEL) {
              Native.imeDelete(1, 0);
            } else {
              Native.imeDelete(0, 1);
            }
          }
          return true;
        }
        // Everything else is the window's business: egui already receives the
        // hardware keys, and answering here too would type them twice.
        return false;
      }
    }

    @Override
    public void onProvideAutofillVirtualStructure(ViewStructure structure, int flags) {
      Log.d(TAG, "autofill provide");
      AutofillId parent = structure.getAutofillId();
      if (parent == null) {
        return;
      }
      structure.addChildCount(2);
      field(
          structure,
          parent,
          0,
          USER,
          View.AUTOFILL_HINT_USERNAME,
          InputType.TYPE_CLASS_TEXT);
      field(
          structure,
          parent,
          1,
          PASSWORD,
          View.AUTOFILL_HINT_PASSWORD,
          InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD);
    }

    private void field(
        ViewStructure structure,
        AutofillId parent,
        int index,
        int id,
        String hint,
        int inputType) {
      ViewStructure node = structure.newChild(index);
      node.setAutofillId(parent, id);
      node.setAutofillHints(new String[] {hint});
      node.setAutofillType(View.AUTOFILL_TYPE_TEXT);
      node.setInputType(inputType);
      // Services use the class name to recognise the node as a text input.
      node.setClassName("android.widget.EditText");
      node.setVisibility(View.VISIBLE);
      Rect rect = rects.get(id);
      if (rect != null) {
        node.setDimens(rect.left, rect.top, 0, 0, rect.width(), rect.height());
      } else {
        // A field that was never on screen still has to occupy something.
        node.setDimens(0, 0, 0, 0, 1, 1);
      }
      node.setAutofillValue(AutofillValue.forText(values.get(id, "")));
    }

    @Override
    public void autofill(SparseArray<AutofillValue> filled) {
      Log.d(TAG, "autofill fill n=" + filled.size());
      String user = null;
      String pass = null;
      for (int i = 0; i < filled.size(); i++) {
        int id = filled.keyAt(i);
        CharSequence text = filled.valueAt(i).getTextValue();
        if (text == null) {
          continue;
        }
        if (id == USER) {
          user = text.toString();
        } else if (id == PASSWORD) {
          pass = text.toString();
        }
      }
      Native.autofillResult(user, pass);
    }
  }

  /** Report the system night mode; the Rust side maps it to egui's theme. */
  private void writeTheme() {
    boolean night =
        (getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK)
            == Configuration.UI_MODE_NIGHT_YES;
    write("theme.txt", night ? "dark" : "light");
  }

  private void write(String name, String value) {
    try {
      java.io.File file = new java.io.File(getFilesDir(), name);
      java.io.FileWriter writer = new java.io.FileWriter(file, false);
      writer.write(value + "\n");
      writer.close();
    } catch (Throwable ignored) {
      // Every reader treats a missing file as "not known yet".
    }
  }
}
