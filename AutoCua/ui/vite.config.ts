// Builds index.tsx (the page, React + TSX) into index.js beside it, as one
// classic script (iife): index.html runs it where it stands, before the
// page's other classic scripts, so the React page is in the DOM when they
// run. `npm run build` here; the build is committed and ships in the wheel
// (pip users have no Node). No source maps: the compiled app's MIME table
// has no .map entry. Vite's root is the package folder (AutoCua/), one up,
// so that this folder is an output folder inside it and is never emptied.
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Vite fills this in for a config file: this folder. The paths below hang
// off it, so a build started from any other folder never writes elsewhere.
declare const __dirname: string;

export default defineConfig({
    root: __dirname + '/..',
    plugins: [react()],
    build: {
        outDir: 'ui',
        emptyOutDir: false,
        copyPublicDir: false,
        sourcemap: false,
        modulePreload: { polyfill: false },
        // the oldest engine the app runs in: WKWebView on macOS 11 (Safari 14)
        target: 'safari14',
        rolldownOptions: {
            input: __dirname + '/index.tsx',
            output: {
                format: 'iife',
                entryFileNames: 'index.js',
                chunkFileNames: '[name].js',
                assetFileNames: '[name][extname]',
            },
        },
    },
});
