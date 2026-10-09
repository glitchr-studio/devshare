// The smallest React Native app that shows what a DevShare session gives
// it: its own JavaScript comes from the host's Metro server, and it calls
// the showcase API by its name, both through the session.

import { StatusBar } from 'expo-status-bar';
import { useEffect, useState } from 'react';
import { Pressable, StyleSheet, Text, View } from 'react-native';

// The showcase of tests/showcase, shared in the same session.
const API = 'http://showcase.test:8711/hello';

export default function App() {
  const [answer, setAnswer] = useState(null);
  const [error, setError] = useState(null);

  async function ask() {
    setError(null);
    try {
      const response = await fetch(API);
      setAnswer(await response.json());
    } catch (caught) {
      setError(String(caught));
    }
  }

  useEffect(() => { ask(); }, []);

  return (
    <View style={styles.screen}>
      <Text style={styles.title}>DevShare</Text>
      <Text style={styles.text}>This app's code came from the host's Metro server, through the session.</Text>
      <Pressable style={styles.button} onPress={ask}>
        <Text style={styles.buttonText}>Call {API}</Text>
      </Pressable>
      {answer && <Text style={styles.mono}>{JSON.stringify(answer, null, 2)}</Text>}
      {error && <Text style={styles.error}>{error}</Text>}
      <StatusBar style="auto" />
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24, gap: 16, backgroundColor: '#fff' },
  title: { fontSize: 28, fontWeight: '600' },
  text: { textAlign: 'center', color: '#555' },
  button: { backgroundColor: '#1c1b1a', paddingVertical: 10, paddingHorizontal: 18, borderRadius: 8 },
  buttonText: { color: '#fff', fontWeight: '600' },
  mono: { fontFamily: 'Menlo', fontSize: 12 },
  error: { color: '#b3261e' },
});
