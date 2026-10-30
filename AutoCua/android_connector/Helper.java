package autocua;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.UiAutomation;
import android.graphics.Bitmap;
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
import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.File;
import java.io.IOException;
import java.io.InputStreamReader;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationTargetException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * The phone helper: takes screenshots and reads the screen's UI tree on
 * request. stream.py starts it on its own as
 *
 *   CLASSPATH=/data/local/tmp/autocua-helper.jar app_process / autocua.Helper SOCKET
 *
 * It keeps the process up itself (its main thread runs the main looper) and
 * ends when its stdin closes.
 *
 * The tree comes through UiAutomation, the API `uiautomator dump` uses, but
 * the connection stays open, so there is no one-second wait for an idle
 * screen before each read. The answer is uiautomator's own XML, so element.rs
 * reads both the same way, plus each node's drawing order. The app's window
 * comes first, then the system's bars, each as one more root node.
 *
 * On the local socket a client sends lines: "dump" (the tree), "shot MAX_EDGE
 * MAX_PIXELS QUALITY" (a JPEG, see shot), "fill ..." (the vault, see fill) or
 * "ping" ("OK" at once, no UiAutomation needed). Each answer is a 4-byte
 * big-endian length, then that many bytes: the XML in UTF-8, the JPEG (it
 * starts FF D8), "OK", or a line starting "ERROR:". Each client gets a thread
 * of its own. Trees and fills take turns (LOCK); screenshots never wait for
 * them.
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

    /** Guards connect, dump and fill, which share WEB, BLANK and the accessibility cache. */
    private static final Object LOCK = new Object();

    /** The thread UiAutomation calls back on, and the one UiAutomation, once connected. */
    private static Looper looper;
    private static volatile UiAutomation ui;

    private Helper() {
    }

    public static void main(String... args) throws Exception {
        // Loaded now, so the jar can go: nothing stays on the phone.
        new File(System.getProperty("java.class.path").split(File.pathSeparator)[0]).delete();
        final String socket = args[0];
        // From Android 15 the accessibility client a connecting UiAutomation
        // builds needs a main looper (AOSP's own uiautomator shell tool had to
        // add the same); this thread runs it, below.
        Looper.prepareMainLooper();
        // The off switch: stream.py holds stdin open and closes it to stop the
        // helper, and it also closes when the computer's side dies. A helper
        // must never outlive its run: it holds Android's one UiAutomation, and
        // the next helper would find it "already registered".
        // Runnables, not lambdas: android.jar has no LambdaMetafactory to compile one against.
        daemon("autocua-lifeline", new Runnable() {
            @Override
            public void run() {
                try {
                    while (System.in.read() >= 0) {
                    }
                } catch (IOException e) {
                    // stdin is gone either way
                }
                System.exit(0);
            }
        });
        daemon("autocua-server", new Runnable() {
            @Override
            public void run() {
                try {
                    serve(socket);
                } catch (Throwable t) {
                    Log.e(TAG, "socket " + socket, t);
                }
                System.exit(1);     // no socket, no helper: stream.py sees the process end
            }
        });
        // Keeps the process up and runs the main looper.
        Looper.loop();
        System.exit(1);
    }

    /** Listens on the socket and answers each client on a thread of its own. */
    private static void serve(String name) throws IOException {
        // Listening before UiAutomation connects: a client that comes early
        // just waits in the queue.
        LocalServerSocket server = new LocalServerSocket(name);
        HandlerThread thread = new HandlerThread("autocua-uiautomation");
        thread.start();
        looper = thread.getLooper();
        try {
            connected();
        } catch (Throwable t) {
            Log.w(TAG, "UiAutomation: " + reason(t));
        }
        while (true) {
            final LocalSocket client = server.accept();
            daemon("autocua-client", new Runnable() {
                @Override
                public void run() {
                    talk(client);
                }
            });
        }
    }

    /** One client: an answer per request line, until it hangs up. */
    private static void talk(LocalSocket client) {
        try (LocalSocket c = client) {
            BufferedReader in = new BufferedReader(
                    new InputStreamReader(c.getInputStream(), StandardCharsets.UTF_8));
            DataOutputStream out = new DataOutputStream(
                    new BufferedOutputStream(c.getOutputStream(), 64 * 1024));
            String line;
            while ((line = in.readLine()) != null) {
                byte[] bytes = answer(line);
                out.writeInt(bytes.length);
                out.write(bytes);
                out.flush();
            }
        } catch (IOException e) {
            // The client went away.
        }
    }

    /** The answer to one request line: bytes, an "ERROR:" line for anything that went wrong. */
    private static byte[] answer(String line) {
        try {
            if (line.equals("ping")) {
                return utf8("OK");      // stream.py's "listening yet?"
            }
            boolean shot = line.startsWith("shot ");
            boolean dump = line.equals("dump");
            boolean fill = line.startsWith("fill ");
            if (!shot && !dump && !fill) {
                return utf8("ERROR: bad_request");
            }
            UiAutomation u = connected();
            if (shot) {
                return shot(u, line);   // outside LOCK: a screenshot never waits for a tree
            }
            synchronized (LOCK) {
                return utf8(dump ? dump(u) : fill(u, line));
            }
        } catch (Throwable t) {
            return utf8("ERROR: " + reason(t));
        }
    }

    /**
     * The UiAutomation, connected on the first request that needs it and
     * tried again on each one while that fails. Android allows one
     * UiAutomation at a time, and another one (Appium, a uiautomator dump, a
     * helper left from a killed run) may have held it so far.
     */
    private static UiAutomation connected() throws Exception {
        UiAutomation now = ui;
        if (now != null) {
            return now;
        }
        synchronized (LOCK) {
            if (ui == null) {
                ui = connect(looper);
            }
            return ui;
        }
    }

    private static void daemon(String name, Runnable body) {
        Thread t = new Thread(body, name);
        t.setDaemon(true);
        t.start();
    }

    private static byte[] utf8(String s) {
        return s.getBytes(StandardCharsets.UTF_8);
    }

    /**
     * "shot MAX_EDGE MAX_PIXELS QUALITY": the screen as a JPEG compressed
     * here, so only the JPEG crosses the cable. It is scaled first so its
     * long side is at most MAX_EDGE and its area at most MAX_PIXELS (0: no
     * cap), each side floored the way ios/tree/element.py does. QUALITY is
     * Bitmap.compress's, 1-100. The answer is the JPEG or a line starting
     * "ERROR:".
     */
    private static byte[] shot(UiAutomation u, String line) {
        String[] parts = line.split(" ");
        int maxEdge, maxPixels, quality;
        try {
            if (parts.length != 4) {
                return utf8("ERROR: bad_request");
            }
            maxEdge = Integer.parseInt(parts[1]);
            maxPixels = Integer.parseInt(parts[2]);
            quality = Integer.parseInt(parts[3]);
        } catch (NumberFormatException e) {
            return utf8("ERROR: bad_request");
        }
        if (maxEdge < 0 || maxPixels < 0 || quality < 1 || quality > 100) {
            return utf8("ERROR: bad_request");
        }
        Bitmap screen = u.takeScreenshot();
        if (screen == null) {
            return utf8("ERROR: no screenshot");
        }
        Bitmap soft = screen;
        Bitmap out = null;
        try {
            // Android 9 to 13 hand over the GPU's own buffer (a HARDWARE
            // bitmap), which can be neither scaled nor compressed: one copy
            // into memory first. Android 14 and up make that copy themselves.
            if (Build.VERSION.SDK_INT >= 26 && screen.getConfig() == Bitmap.Config.HARDWARE) {
                soft = screen.copy(Bitmap.Config.ARGB_8888, false);
                if (soft == null) {
                    return utf8("ERROR: the screenshot could not be copied from the GPU");
                }
            }
            int w = soft.getWidth();
            int h = soft.getHeight();
            double scale = 1.0;
            if (maxEdge > 0) {
                scale = Math.min(scale, (double) maxEdge / Math.max(w, h));
            }
            if (maxPixels > 0) {
                scale = Math.min(scale, Math.sqrt((double) maxPixels / ((double) w * h)));
            }
            out = scale < 1.0
                    ? Bitmap.createScaledBitmap(soft, Math.max(1, (int) (w * scale)), Math.max(1, (int) (h * scale)), true)
                    : soft;
            ByteArrayOutputStream jpeg = new ByteArrayOutputStream(512 * 1024);
            if (!out.compress(Bitmap.CompressFormat.JPEG, quality, jpeg)) {
                return utf8("ERROR: the screenshot could not be compressed");
            }
            return jpeg.toByteArray();
        } finally {
            // About 10 MB of native memory each, several a second with the
            // preview: freed now, not at the next GC.
            if (out != null && out != soft) {
                out.recycle();
            }
            if (soft != null && soft != screen) {
                soft.recycle();
            }
            screen.recycle();
        }
    }

    /**
     * A new UiAutomation, connected the way `uiautomator dump --compressed`
     * connects one (a failed one cannot be connected again, so each try makes
     * its own). main() prepared the main looper before this thread started.
     */
    private static UiAutomation connect(Looper looper) throws Exception {
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
