import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  // The daemon serves plain files from the build root, so every URL is relative.
  base: './',
  plugins: [tailwindcss(), svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  test: {
    include: ['src/data/**/*.test.ts', 'src/lib/**/*.test.ts'],
  },
});
