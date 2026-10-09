# A React Native app shared with DevShare

The smallest Expo app, served by its Metro dev server, to show that a
DevShare session can carry a native app's code to a phone the way it
carries a web site: by name, over the session, from any network.

```sh
npm install
npx expo start --port 8081      # Metro, on this machine
devshare share .                # shares metro.test:8081, from the next folder up: tests/react-native
```

`devshare discover` reads `package.json`: a project that depends on
`react-native` or `expo` has a Metro server on port 8081, declared with
`kind = "metro"`, and a way to open it with Expo Go, `exp://<name>:8081`.
The DevShare apps show that to the guest.

What a phone needs to run it: a React Native runtime that loads the bundle
through the session. Expo Go is one, and it reaches the session's names only
through a system-wide tunnel on the phone, which is DevShare's packet-tunnel
extension (not written yet; the app's own browser does not help here).
Until then this folder is read by the tests, and runs on a computer with
Expo Go in a simulator and the Mac in the session.
