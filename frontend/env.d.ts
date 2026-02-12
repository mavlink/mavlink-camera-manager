/// <reference types="vite/client" />

// Viz is loaded from CDN in index.html
declare class Viz {
  renderString(src: string): Promise<string>;
}
