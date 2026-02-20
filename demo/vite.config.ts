import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import { nodePolyfills } from 'vite-plugin-node-polyfills'

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')

  const writeUrl = env.VITE_WRITE_URL || 'http://localhost:8899'
  const readUrl = env.VITE_READ_URL || 'http://localhost:8899'

  return {
    plugins: [
      react(),
      nodePolyfills({
        globals: {
          Buffer: true,
          global: true,
          process: true,
        },
        protocolImports: true,
      }),
    ],
    define: {
      'process.env': {},
    },
    server: {
      proxy: {
        '/api/write': {
          target: writeUrl,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/api\/write/, ''),
        },
        '/api/read': {
          target: readUrl,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/api\/read/, ''),
        },
      },
    },
  }
})
