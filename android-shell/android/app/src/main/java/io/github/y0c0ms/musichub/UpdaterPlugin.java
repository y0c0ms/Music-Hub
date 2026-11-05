package io.github.y0c0ms.musichub;

import android.content.Intent;
import android.net.Uri;
import android.provider.Settings;
import android.os.Build;

import androidx.core.content.FileProvider;

import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/**
 * Downloads a new APK and hands it to Android's package installer.
 *
 * Three details make this worth writing rather than opening a browser link:
 *
 *  1. Only GitHub release hosts are contacted, over HTTPS, on every redirect
 *     hop. The URL comes from the web page, and the page is served by whatever
 *     server the user configured - so the native side refuses to install
 *     anything that does not come from a GitHub release.
 *  2. The file goes to the app's private cache directory, not Downloads. It is
 *     an implementation detail of an update, not a document the user wanted, and
 *     leaving stale APKs in Downloads is exactly what was asked to be avoided.
 *  3. Whatever happens, the file is removed afterwards: on success once the
 *     installer has taken it, and on the next launch if the install was
 *     cancelled. Nothing accumulates.
 */
@CapacitorPlugin(name = "Updater")
public class UpdaterPlugin extends Plugin {
    private static final String APK_NAME = "update.apk";
    private static final int MAX_REDIRECTS = 5;

    /** GitHub serves release assets from github.com and redirects to a CDN host. */
    private static final Set<String> ALLOWED_HOSTS = new HashSet<>(Arrays.asList(
            "github.com",
            "objects.githubusercontent.com",
            "release-assets.githubusercontent.com"));

    @Override
    public void load() {
        // A cancelled or crashed install leaves the previous download behind.
        // Clearing at startup means at most one stale file ever exists, and only
        // until the app is next opened.
        clearStaleApk();
    }

    /** Whether Android will let this app hand an APK to the installer. */
    @PluginMethod
    public void canInstall(PluginCall call) {
        JSObject ret = new JSObject();
        ret.put("granted", mayInstall());
        call.resolve(ret);
    }

    private boolean mayInstall() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return true;
        }
        return getContext().getPackageManager().canRequestPackageInstalls();
    }

    /**
     * Send the user to the one settings page that grants it.
     *
     * Without this the install intent is simply swallowed: no dialog, no error,
     * nothing happens - which is indistinguishable from a broken button.
     */
    private void requestInstallPermission() {
        Intent intent = new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                Uri.parse("package:" + getContext().getPackageName()));
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        getContext().startActivity(intent);
    }

    @PluginMethod
    public void downloadAndInstall(PluginCall call) {
        final String url = call.getString("url");
        if (url == null || url.isEmpty()) {
            call.reject("url is required");
            return;
        }
        if (!isAllowed(url)) {
            call.reject("updates are only downloaded from GitHub releases");
            return;
        }

        if (!mayInstall()) {
            requestInstallPermission();
            call.reject("Allow this app to install unknown apps, then press Update again");
            return;
        }
        // Off the main thread: this is several megabytes over the network.
        new Thread(() -> {
            File apk = new File(getContext().getCacheDir(), APK_NAME);
            try {
                download(url, apk);
            } catch (Exception e) {
                //noinspection ResultOfMethodCallIgnored
                apk.delete();
                call.reject("download failed: " + e.getMessage());
                return;
            }

            try {
                Uri uri = FileProvider.getUriForFile(
                        getContext(), getContext().getPackageName() + ".fileprovider", apk);

                Intent intent = new Intent(Intent.ACTION_VIEW);
                intent.setDataAndType(uri, "application/vnd.android.package-archive");
                // The installer is a different app, so it needs explicit
                // permission to read a file inside this one's private cache.
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                getContext().startActivity(intent);

                JSObject ret = new JSObject();
                ret.put("started", true);
                ret.put("bytes", apk.length());
                call.resolve(ret);
            } catch (Exception e) {
                //noinspection ResultOfMethodCallIgnored
                apk.delete();
                call.reject("could not open the installer: " + e.getMessage());
            }
        }).start();
    }

    /** HTTPS to an allow-listed GitHub host, nothing else. */
    static boolean isAllowed(String url) {
        try {
            URL u = new URL(url);
            return "https".equals(u.getProtocol())
                    && ALLOWED_HOSTS.contains(u.getHost().toLowerCase(Locale.ROOT));
        } catch (Exception e) {
            return false;
        }
    }

    private void download(String url, File dest) throws Exception {
        // Redirects are followed by hand so every hop is checked against the
        // allow-list; HttpURLConnection would otherwise follow them blindly.
        String current = url;
        HttpURLConnection conn = null;
        for (int hop = 0; ; hop++) {
            if (!isAllowed(current)) {
                throw new Exception("refusing to download from " + current);
            }
            conn = (HttpURLConnection) new URL(current).openConnection();
            conn.setConnectTimeout(20000);
            conn.setReadTimeout(120000);
            conn.setInstanceFollowRedirects(false);
            conn.connect();
            int code = conn.getResponseCode();
            if (code >= 300 && code < 400) {
                String location = conn.getHeaderField("Location");
                conn.disconnect();
                if (location == null || hop >= MAX_REDIRECTS) {
                    throw new Exception("too many or empty redirects");
                }
                current = new URL(new URL(current), location).toString();
                continue;
            }
            if (code != HttpURLConnection.HTTP_OK) {
                conn.disconnect();
                throw new Exception("server returned HTTP " + code);
            }
            break;
        }

        try (InputStream in = conn.getInputStream(); FileOutputStream out = new FileOutputStream(dest)) {
            byte[] buf = new byte[64 * 1024];
            int n;
            while ((n = in.read(buf)) != -1) {
                out.write(buf, 0, n);
            }
            out.flush();
        } finally {
            conn.disconnect();
        }

        // A truncated download would be rejected by the installer with a far
        // less helpful message than this one.
        if (dest.length() < 1024) {
            throw new Exception("downloaded file is too small to be an apk");
        }
    }

    private void clearStaleApk() {
        File apk = new File(getContext().getCacheDir(), APK_NAME);
        if (apk.exists()) {
            //noinspection ResultOfMethodCallIgnored
            apk.delete();
        }
    }
}
