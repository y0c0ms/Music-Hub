package io.github.y0c0ms.musichub;

import android.os.Bundle;

import androidx.core.content.ContextCompat;

import com.getcapacitor.BridgeActivity;
import com.getcapacitor.CapConfig;
import com.getcapacitor.Logger;

import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;

public class MainActivity extends BridgeActivity {
    /** Local page shown when the configured server cannot be reached. */
    static final String OFFLINE_PATH = "index.html?offline=1";

    @Override
    public void onCreate(Bundle savedInstanceState) {
        // Local plugins are not discovered automatically the way node_modules
        // ones are; without this they resolve to nothing at runtime.
        registerPlugin(UpdaterPlugin.class);
        registerPlugin(ServerConfigPlugin.class);

        // The server URL is chosen by the user on first launch, so it cannot live
        // in capacitor.config.json. With no URL saved the bundled setup page
        // (www/index.html) loads; once one is saved the WebView loads the server
        // directly and falls back to the same page in offline mode.
        String serverUrl = ServerConfigPlugin.serverUrl(this);
        if (serverUrl != null) {
            CapConfig withServer = configFor(serverUrl);
            if (withServer != null) {
                config = withServer;
            }
        }

        super.onCreate(savedInstanceState);

        // Repaint the WebView in the theme's own colour.
        //
        // capacitor.config.json can only carry ONE backgroundColor, and it is
        // applied to the WebView after the bridge starts. With a single value that
        // is a dark flash on every launch for anyone using the light theme, which
        // is the whole reason this override exists. @color/appBackground resolves
        // through values/ and values-night/, so the shell matches the page instead
        // of contradicting it for the moment before first paint.
        if (getBridge() != null && getBridge().getWebView() != null) {
            getBridge().getWebView().setBackgroundColor(
                    ContextCompat.getColor(this, R.color.appBackground));
        }
    }

    /**
     * The bundled capacitor.config.json with `server.url` pointed at the saved
     * server. Returns null (bundled config unchanged) if the asset cannot be read.
     */
    private CapConfig configFor(String serverUrl) {
        try (InputStream in = getAssets().open("capacitor.config.json")) {
            ByteArrayOutputStream buf = new ByteArrayOutputStream();
            byte[] chunk = new byte[8192];
            int n;
            while ((n = in.read(chunk)) != -1) {
                buf.write(chunk, 0, n);
            }
            JSONObject json = new JSONObject(buf.toString(StandardCharsets.UTF_8.name()));
            JSONObject server = json.optJSONObject("server");
            if (server == null) {
                server = new JSONObject();
                json.put("server", server);
            }
            server.put("url", serverUrl);
            server.put("errorPath", OFFLINE_PATH);

            File dir = new File(getFilesDir(), "runtime-config");
            if (!dir.isDirectory() && !dir.mkdirs()) {
                throw new IllegalStateException("cannot create " + dir);
            }
            try (FileOutputStream out = new FileOutputStream(new File(dir, "capacitor.config.json"))) {
                out.write(json.toString().getBytes(StandardCharsets.UTF_8));
            }
            return CapConfig.loadFromFile(this, dir.getAbsolutePath());
        } catch (Exception e) {
            Logger.error("Could not apply the saved server URL", e);
            return null;
        }
    }
}
