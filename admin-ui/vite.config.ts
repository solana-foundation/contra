import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import path from 'path'

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')

  return {
    plugins: [react()],
    resolve: {
      alias: {
        '@private-channel-escrow': path.resolve(__dirname, '../private-channel-escrow-program/clients/typescript/src/generated'),
        '@private-channel-withdraw': path.resolve(__dirname, '../private-channel-withdraw-program/clients/typescript/src/generated'),
      },
    },
    define: {
      global: 'globalThis',
      'process.env': {},
      'process.env.NODE_ENV': JSON.stringify(process.env.NODE_ENV || 'development'),
      'import.meta.env.VITE_PRIVATE_CHANNEL_RPC_URL': JSON.stringify(env.PRIVATE_CHANNEL_RPC_URL || 'https://api.example.com'),
    },
  }
})
