# Music Hub Android app

A [Capacitor](https://capacitorjs.com/) wrapper around a Music Hub server's PWA.
The web UI is not duplicated here: the WebView loads the live site from your
server, so the app and the browser always run the same build.

Prebuilt APKs are attached to
[GitHub Releases](https://github.com/y0c0ms/Music-Hub/releases). Once installed,
the app offers updates from the same place (Settings → App).

## Server URL

The APK contains no server address. On first launch it shows a small setup page
(`www/index.html`) asking for your server's `https://` URL. The URL is stored in
the app's private preferences and, from then on, the app opens the server
directly.

- **Change it later:** Settings → Server → *Change server*, or *Change server*
  on the "Can't reach Music Hub" page shown when the server is unreachable.
- **HTTPS only.** Cleartext is disabled in `network_security_config.xml`.
- **Sign-in stays in the app.** When the server redirects to its OIDC provider
  (an authorization request whose `redirect_uri` points back at your server),
  that provider's host is kept inside the WebView instead of being handed to the
  system browser, so the session cookie set on return reaches the app.

Loading the server rather than bundling `www/` keeps the WebView **same-origin**
with the API, so the HttpOnly `SameSite=Lax` session cookie works untouched.
Bundling the UI would make every API call cross-origin and force
`SameSite=None` plus CORS.

## Build

Requirements: Node.js 22, JDK 21 and the Android SDK (`ANDROID_HOME` set).

```bash
cd android-shell
npm ci
npx cap sync android
cd android && ./gradlew assembleDebug
# -> android/app/build/outputs/apk/debug/app-debug.apk
```

### Release signing

`./gradlew assembleRelease` signs the APK when `MH_KEYSTORE_PROPERTIES` points
at a properties file outside the repository:

```properties
storeFile=/path/to/release.jks
storePassword=...
keyAlias=...
keyPassword=...
```

Without it the release build is unsigned. Keep the keystore safe: Android
refuses to install an update signed by a different key.

## Servers behind a private CA

The app trusts only the system CA store. If your server's certificate is issued
by a private CA, either:

1. **Bundle the CA in your own build** (recommended): copy the PEM certificate to
   `android/app/src/main/res/raw/my_ca.crt` and add a domain block to
   `android/app/src/main/res/xml/network_security_config.xml`:

   ```xml
   <domain-config cleartextTrafficPermitted="false">
       <domain includeSubdomains="true">music.example.internal</domain>
       <domain includeSubdomains="true">auth.example.internal</domain>
       <trust-anchors>
           <certificates src="@raw/my_ca" />
           <certificates src="system" />
       </trust-anchors>
   </domain-config>
   ```

   List the OIDC provider's host too if it uses the same CA. Keep
   `<certificates src="system" />` alongside the bundled CA - replacing the system
   store breaks every public host.

2. **Trust user-installed CAs**: install the CA on the device (Settings →
   Security → Encryption & credentials) and add `<certificates src="user" />` to
   the `base-config` trust anchors. This lets any user-installed certificate
   intercept the app's traffic, so it is off by default.

## In-app updates

Settings → App reads
`https://api.github.com/repos/y0c0ms/Music-Hub/releases/latest`, picks the
release's `.apk` asset, and the native `UpdaterPlugin` downloads it into the
app's private cache and opens the system installer. Downloads are accepted only
over HTTPS from `github.com`, `objects.githubusercontent.com` and
`release-assets.githubusercontent.com`, on every redirect hop. Android asks the
user to allow "install unknown apps" for this app once.

Keep `versionName` in `android/app/build.gradle` in step with the release tag:
the updater compares the two.
