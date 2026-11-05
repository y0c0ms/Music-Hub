package io.github.y0c0ms.musichub;

import android.content.Context;
import android.content.SharedPreferences;
import android.net.Uri;

import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

import java.util.HashSet;
import java.util.Set;

/**
 * Owns the user-chosen server URL and keeps sign-in inside the WebView.
 *
 * The bundled pages (www/index.html) talk to this plugin by NAVIGATING to
 * `https://localhost/_musichub/...` rather than through the JS bridge: once a
 * server URL is set, Capacitor only injects its bridge into the server's origin,
 * so the local offline page has no bridge to call. Navigations reach
 * {@link #shouldOverrideLoad} either way.
 *
 * <ul>
 *   <li>{@code /_musichub/set?url=https://...} saves the URL and restarts the
 *       activity so the WebView loads it.</li>
 *   <li>{@code /_musichub/reset} forgets it and restarts into the setup page.</li>
 *   <li>{@code /_musichub/retry} restarts, trying the saved server again.</li>
 * </ul>
 */
@CapacitorPlugin(name = "ServerConfig")
public class ServerConfigPlugin extends Plugin {
    private static final String PREFS = "music_hub_server";
    private static final String KEY_URL = "server_url";
    private static final String COMMAND_PREFIX = "/_musichub/";

    /**
     * Hosts of the OIDC provider, learned when the server redirects to it.
     * Capacitor would otherwise treat a sign-in page on another host as an
     * external link and open it in the system browser, where the session cookie
     * set on return never reaches the app.
     */
    private final Set<String> authHosts = new HashSet<>();

    /** The saved server URL, or null before first-launch setup. */
    static String serverUrl(Context context) {
        String url = prefs(context).getString(KEY_URL, null);
        return isValidServerUrl(url) ? url : null;
    }

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /** HTTPS only: the network security config forbids cleartext anyway. */
    static boolean isValidServerUrl(String url) {
        if (url == null || url.isEmpty()) {
            return false;
        }
        Uri uri = Uri.parse(url);
        return "https".equals(uri.getScheme()) && uri.getHost() != null && !uri.getHost().isEmpty();
    }

    @Override
    public Boolean shouldOverrideLoad(Uri url) {
        if (getBridge().getHost().equals(url.getHost())
                && url.getPath() != null
                && url.getPath().startsWith(COMMAND_PREFIX)) {
            handleCommand(url);
            return true;
        }

        String server = serverUrl(getContext());
        if (server == null || !url.isHierarchical() || !"https".equals(url.getScheme())) {
            return null;
        }
        // An authorization request names the server's callback as redirect_uri;
        // its host is the OIDC provider, which must stay inside the WebView.
        String redirect = url.getQueryParameter("redirect_uri");
        String serverHost = Uri.parse(server).getHost();
        if (redirect != null && serverHost != null && serverHost.equals(Uri.parse(redirect).getHost())) {
            authHosts.add(url.getHost());
            return false;
        }
        if (authHosts.contains(url.getHost())) {
            return false;
        }
        return null;
    }

    /** Forget the server and return to the setup page; called from Settings. */
    @PluginMethod
    public void reset(PluginCall call) {
        prefs(getContext()).edit().remove(KEY_URL).commit();
        call.resolve();
        getActivity().runOnUiThread(() -> getActivity().recreate());
    }

    private void handleCommand(Uri url) {
        String action = url.getPath().substring(COMMAND_PREFIX.length());
        if (action.equals("set")) {
            String server = trimSlash(url.getQueryParameter("url"));
            if (!isValidServerUrl(server)) {
                return;
            }
            prefs(getContext()).edit().putString(KEY_URL, server).commit();
        } else if (action.equals("reset")) {
            prefs(getContext()).edit().remove(KEY_URL).commit();
        } else if (!action.equals("retry")) {
            return;
        }
        // The server URL is read once, when the bridge is built; a fresh
        // activity is the only way to apply a new one.
        getActivity().runOnUiThread(() -> getActivity().recreate());
    }

    private static String trimSlash(String s) {
        if (s == null) {
            return null;
        }
        String out = s.trim();
        while (out.endsWith("/")) {
            out = out.substring(0, out.length() - 1);
        }
        return out;
    }
}
