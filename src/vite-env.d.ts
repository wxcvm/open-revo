/// <reference types="vite/client" />

declare module '*.jpg' {
  const src: string;
  export default src;
}

declare module '*.png' {
  const src: string;
  export default src;
}

declare const __APP_VERSION__: string;
declare const __BUILD_TIME__: string;
declare const __RAW_VERSION__: string;
declare const __GIT_COMMIT__: string;
