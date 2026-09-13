import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    // Tauri compiles into src-tauri/target while Vite is running. Watching
    // that directory causes Windows EBUSY errors when the DLL is replaced.
    // Atomic-write tools leave short-lived *.tmpdir/*.tmp files inside src;
    // Node's watcher throws EBUSY on those and kills the dev server.
    watch: {
      ignored: ['**/src-tauri/**', '**/data/**', '**/*.tmpdir/**', '**/*.tmp'],
    },
  },
})
