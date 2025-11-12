! function () {
    "use strict";
    async function n(n, i = {}, o) {
        return window.__TAURI_INTERNALS__.invoke(n, i, o);
    }
}();