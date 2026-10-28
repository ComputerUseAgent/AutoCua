package autocua;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.graphics.Rect;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;
import android.os.Bundle;
import android.os.HandlerThread;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Base64;
import android.util.Log;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

import java.io.BufferedOutputStream;
import java.io.BufferedReader;
import java.io.DataOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * AutoCua's half of the phone helper: reads the screen's UI tree on request.
 *
 * It runs inside scrcpy's server process, so the phone has one helper that
 * both streams the screen and reads the tree. stream.py starts it as
 *
 *   CLASSPATH=scrcpy-server.jar:autocua-helper.jar app_process / autocua.Helper SOCKET SCRCPY_ARGS...
 *
 * scrcpy's jar must come first: scrcpy takes the first classpath entry as its
 * own file (it deletes it, and runs its clean-up from it).
 *
 * The tree comes through UiAutomation, the API `uiautomator dump` uses, but
 * the connection stays open, so there is no one-second wait for an idle
 * screen before each read. The answer is uiautomator's own XML, so element.rs
 * reads both the same way, plus each node's drawing order. The app's window
 * comes first, then the system's bars, each as one more root node.
 *
 * On the local socket: the client sends a line, "dump" for the tree or
 * "fill ..." for the vault (see fill); the answer is a 4-byte big-endian
 * length, then that many bytes of UTF-8, either the XML ("OK" for a fill) or
 * a line starting with "ERROR:".
 */
public final class Helper {

    private static final String TAG = "AutoCua";

    /** Longest wait for an empty web page to fill in (see dump). */
    private static final long WEB_WAIT_MS = 2000;

    /** How long the field the vault tapped gets to take input focus (see fill). */
    private static final long FOCUS_WAIT_MS = 1500;
    private static final long FOCUS_POLL_MS = 200;

    /** Boxes seen as empty web pages, and those that stayed empty a whole WEB_WAIT_MS. */
    private static final Set<Integer> WEB = new HashSet<Integer>();
    private static final Set<Integer> BLANK = new HashSet<Integer>();

    private Helper() {
    }

    public static void main(String... args) throws Exception {
        // Loaded now, so the file can go, as scrcpy removes its own.
        String[] classPath = System.getProperty("java.class.path").split(File.pathSeparator);
        if (classPath.length > 1) {
            new File(classPath[1]).delete();
        }
        final String socket = args[0];
        // A Runnable, not a lambda: android.jar has no LambdaMetafactory to compile one against.
        Thread tree = new Thread(new Runnable() {
            @Override
            public void run() {
                serve(socket);
            }
        }, "autocua-tree");
        tree.setDaemon(true);
        tree.start();
        // scrcpy streams the screen on this thread and ends the whole process
        // when its video client goes away, which ends the tree reader too.
        Class.forName("com.genymobile.scrcpy.Server")
                .getMethod("main", String[].class)
                .invoke(null, (Object) Arrays.copyOfRange(args, 1, args.length));
    }

    /** Answers one client at a time, one answer per request line. */
    private static void serve(String name) {
        // Listening before UiAutomation connects: a client that comes early
        // just waits in the queue.
        try (LocalServerSocket server = new LocalServerSocket(name)) {
            HandlerThread thread = new HandlerThread("autocua-uiautomation");
            thread.start();
            UiAutomation ui = null;
            try {
                ui = connect(thread.getLooper());
            } catch (Throwable t) {
                Log.w(TAG, "UiAutomation: " + reason(t));
            }
            while (true) {
                try (LocalSocket client = server.accept()) {
                    BufferedReader in = new BufferedReader(
                            new InputStreamReader(client.getInputStream(), StandardCharsets.UTF_8));
                    DataOutputStream out = new DataOutputStream(
                            new BufferedOutputStream(client.getOutputStream(), 64 * 1024));
                    String line;
                    while ((line = in.readLine()) != null) {
                        String answer;
                        try {
                            // Not connected yet: try again. Android allows one
                            // UiAutomation at a time, and another one (Appium, a
                            // uiautomator dump, a helper left from a killed run) may
                            // have held it so far.
                            if (ui == null) {
                                ui = connect(thread.getLooper());
                            }
                            answer = line.startsWith("fill ") ? fill(ui, line) : dump(ui);
                        } catch (Throwable t) {
                            answer = "ERROR: " + reason(t);
                        }
                        byte[] bytes = answer.getBytes(StandardCharsets.UTF_8);
                        out.writeInt(bytes.length);
                        out.write(bytes);
                        out.flush();
                    }
                } catch (IOException e) {
                    // The client went away; wait for the next one.
                }
            }
        } catch (IOException e) {
            Log.e(TAG, "tree socket " + name, e);
        }
    }

    /**
     * A new UiAutomation, connected the way `uiautomator dump --compressed`
     * connects one (a failed one cannot be connected again, so each try makes
     * its own).
     */
    private static UiAutomation connect(Looper looper) throws Exception {
        // From Android 15 the accessibility client a connecting UiAutomation
        // builds needs a main looper, and here only scrcpy prepares one, on the
        // main thread. AOSP's own uiautomator shell tool had to add the same.
        for (int i = 0; Looper.getMainLooper() == null && i < 500; i++) {
            Thread.sleep(10);
        }
        // Hidden API, reachable from app_process (not from an app).
        Object connection = Class.forName("android.app.UiAutomationConnection")
                .getDeclaredConstructor().newInstance();
        Constructor<UiAutomation> make = UiAutomation.class.getDeclaredConstructor(
                Looper.class, Class.forName("android.app.IUiAutomationConnection"));
        make.setAccessible(true);
        UiAutomation ui = make.newInstance(looper, connection);
        if (Build.VERSION.SDK_INT >= 24) {
            // Leave the user's own accessibility services (screen readers,
            // password managers) running.
            UiAutomation.class.getMethod("connect", int.class)
                    .invoke(ui, UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES);
        } else {
            UiAutomation.class.getMethod("connect").invoke(ui);
        }
        // Only the views that matter to accessibility (layout boxes with no
        // text are left out), with their resource ids, and every window on
        // the screen, not only the app's (the system's bars are their own).
        AccessibilityServiceInfo info = ui.getServiceInfo();
        info.flags &= ~AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS;
        info.flags |= AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS
                | AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS;
        ui.setServiceInfo(info);
        return ui;
    }

    /** What went wrong, from under the reflection wrapper, in one line. */
    private static String reason(Throwable t) {
        while (t instanceof InvocationTargetException && t.getCause() != null) {
            t = t.getCause();
        }
        String text = String.valueOf(t);
        if (text.contains("already registered")) {
            text += " (another UiAutomation is connected: Appium, a uiautomator dump,"
                    + " or a helper left from an earlier run)";
        }
        return text.replace('\n', ' ');
    }

    /**
     * The vault's fill: "fill PASSWORD PACKAGE TEXT", PASSWORD 1 when the
     * value is a password, PACKAGE the app the scan read and TEXT the value,
     * both in base64 (so neither can break the line). Waits up to
     * FOCUS_WAIT_MS for the field with input focus (the
     * vault's own tap put it there), checks it may take this value (a
     * password only into a password field, anything only into an editable
     * one, and only inside the app that was scanned, never a system screen
     * that came up since), then sets the whole text at once
     * (ACTION_SET_TEXT): any characters, no keyboard. The answer is "OK" or
     * "ERROR: <why> [<class>]"; neither it nor the log ever carries the text.
     */
    private static String fill(UiAutomation ui, String line) {
        String[] parts = line.split(" ", 4);
        if (parts.length != 4) {
            return "ERROR: bad_request";
        }
        AccessibilityNodeInfo field = null;
        try {
            long until = SystemClock.uptimeMillis() + FOCUS_WAIT_MS;
            while (true) {
                clearCache(ui);
                field = ui.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
                if (field != null || SystemClock.uptimeMillis() >= until) {
                    break;
                }
                Thread.sleep(FOCUS_POLL_MS);
            }
            if (field == null) {
                return "ERROR: no_focus";
            }
            String kind = String.valueOf(field.getClassName());
            if ("1".equals(parts[1]) && !field.isPassword()) {
                return "ERROR: not_password " + kind;
            }
            if (!field.isEditable()) {
                return "ERROR: not_text " + kind;
            }
            String scanned = new String(Base64.decode(parts[2], Base64.DEFAULT), StandardCharsets.UTF_8);
            if (!scanned.contentEquals(String.valueOf(field.getPackageName()))) {
                return "ERROR: wrong_app";
            }
            Bundle text = new Bundle();
            text.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                    new String(Base64.decode(parts[3], Base64.DEFAULT), StandardCharsets.UTF_8));
            return field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, text) ? "OK" : "ERROR: refused " + kind;
        } catch (Throwable t) {
            // Never the exception's own text: it could quote what was being set.
            return "ERROR: failed";
        } finally {
            if (field != null) {
                field.recycle();
            }
        }
    }

    /**
     * The tree, read again while a web page in it is still empty. Browsers
     * built on older Chrome code (JioPages, measured) make a page's tree only
     * once something asks for it: the first read finds one empty "Web View"
     * box, and the whole page follows about 250 ms later.
     */
    private static String dump(UiAutomation ui) throws InterruptedException {
        long until = SystemClock.uptimeMillis() + WEB_WAIT_MS;
        while (true) {
            List<Integer> empty = new ArrayList<Integer>();
            String xml = read(ui, empty);
            if (empty.isEmpty()) {
                return xml;
            }
            if (SystemClock.uptimeMillis() >= until) {
                BLANK.addAll(empty);    // blank pages: never waited for again
                return xml;
            }
            Thread.sleep(50);
        }
    }

    /** One read of every window; empty web page boxes are added to `empty`. */
    @SuppressWarnings("deprecation")
    private static String read(UiAutomation ui, List<Integer> empty) {
        clearCache(ui);
        AccessibilityNodeInfo root = ui.getRootInActiveWindow();
        if (root == null) {
            return "ERROR: no active window";
        }
        StringBuilder xml = new StringBuilder(64 * 1024);
        xml.append("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy>");
        Rect box = new Rect();
        node(xml, root, 0, box, empty);
        int app = root.getWindowId();
        root.recycle();
        // Then the system's own windows on the screen, each as one more root:
        // the status bar, the bar with Back / Home / Recent apps. A phone that
        // uses gestures has no button bar, so nothing comes for it. Bottom to
        // top: getWindows lists the top one first, and element.rs takes a
        // later root as drawn over the ones before it.
        List<AccessibilityWindowInfo> windows = ui.getWindows();
        for (int i = windows.size() - 1; i >= 0; i--) {
            AccessibilityWindowInfo window = windows.get(i);
            if (window.getType() == AccessibilityWindowInfo.TYPE_SYSTEM && window.getId() != app) {
                AccessibilityNodeInfo bar = window.getRoot();
                if (bar != null) {
                    node(xml, bar, 0, box, empty);
                    bar.recycle();
                }
            }
            window.recycle();
        }
        xml.append("</hierarchy>");
        return xml.toString();
    }

    /**
     * Empties the accessibility cache, so the next read asks the apps afresh.
     * The cache only hears of changes through accessibility events, and not
     * every app sends them: on a kept-open connection a home-screen clock
     * widget still read 1:35 at 1:36 (measured). A fresh `uiautomator dump`
     * never saw this, as it starts with an empty cache every time.
     */
    private static void clearCache(UiAutomation ui) {
        try {
            try {
                UiAutomation.class.getMethod("clearCache").invoke(ui);    // Android 13 and up
                return;
            } catch (NoSuchMethodException e) {
                // older: the cache lives in AccessibilityInteractionClient
            }
            Class<?> client = Class.forName("android.view.accessibility.AccessibilityInteractionClient");
            Object instance = client.getMethod("getInstance").invoke(null);
            try {
                client.getMethod("clearCache").invoke(instance);          // up to Android 11: one cache
            } catch (NoSuchMethodException e) {
                Field id = UiAutomation.class.getDeclaredField("mConnectionId");
                id.setAccessible(true);
                client.getMethod("clearCache", int.class).invoke(instance, id.getInt(ui));   // Android 12: one per connection
            }
        } catch (Throwable t) {
            Log.w(TAG, "could not clear the accessibility cache", t);
        }
    }

    /** One node in uiautomator's format, then its children that are visible to the user. */
    @SuppressWarnings("deprecation")
    private static void node(StringBuilder xml, AccessibilityNodeInfo n, int index, Rect box, List<Integer> empty) {
        n.getBoundsInScreen(box);
        xml.append("<node index=\"").append(index).append('"');
        attr(xml, "text", n.getText());
        attr(xml, "resource-id", n.getViewIdResourceName());
        attr(xml, "class", n.getClassName());
        attr(xml, "package", n.getPackageName());
        attr(xml, "content-desc", n.getContentDescription());
        flag(xml, "checkable", n.isCheckable());
        flag(xml, "checked", n.isChecked());
        flag(xml, "clickable", n.isClickable());
        flag(xml, "enabled", n.isEnabled());
        flag(xml, "focusable", n.isFocusable());
        flag(xml, "focused", n.isFocused());
        flag(xml, "scrollable", n.isScrollable());
        flag(xml, "long-clickable", n.isLongClickable());
        flag(xml, "password", n.isPassword());
        flag(xml, "selected", n.isSelected());
        xml.append(" bounds=\"[").append(box.left).append(',').append(box.top)
                .append("][").append(box.right).append(',').append(box.bottom).append("]\"");
        if (Build.VERSION.SDK_INT >= 24) {
            xml.append(" drawing-order=\"").append(n.getDrawingOrder()).append('"');
        }
        xml.append('>');
        int shown = 0;
        for (int i = 0; i < n.getChildCount(); i++) {
            AccessibilityNodeInfo child = n.getChild(i);
            if (child == null) {
                continue;
            }
            if (child.isVisibleToUser()) {
                node(xml, child, i, box, empty);
                shown++;
            }
            child.recycle();
        }
        xml.append("</node>");
        // A web page's box with nothing in it: a WebView, or the box browsers
        // built on Chrome label "Web View". The label goes once the page's
        // tree is on its way (measured), so a box seen once keeps counting;
        // the same view keeps its hash.
        int id = n.hashCode();
        if (shown == 0 && !BLANK.contains(id) && (WEB.contains(id)
                || String.valueOf(n.getClassName()).contains("WebView")
                || "Web View".contentEquals(String.valueOf(n.getContentDescription())))) {
            WEB.add(id);
            empty.add(id);
        }
    }

    private static void attr(StringBuilder xml, String name, CharSequence value) {
        xml.append(' ').append(name).append("=\"");
        if (value != null) {
            for (int i = 0; i < value.length(); i++) {
                char c = value.charAt(i);
                if (c == '&') {
                    xml.append("&amp;");
                } else if (c == '<') {
                    xml.append("&lt;");
                } else if (c == '>') {
                    xml.append("&gt;");
                } else if (c == '"') {
                    xml.append("&quot;");
                } else if ((c < 0x20 && c != '\t' && c != '\n' && c != '\r') || c == 0xFFFE || c == 0xFFFF) {
                    xml.append(' ');    // XML has no room for these
                } else {
                    xml.append(c);
                }
            }
        }
        xml.append('"');
    }

    private static void flag(StringBuilder xml, String name, boolean value) {
        xml.append(' ').append(name).append("=\"").append(value).append('"');
    }
}
