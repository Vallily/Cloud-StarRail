const invoke = (action) => window.__TAURI_INTERNALS__.invoke('window_action', {action}).catch(console.error);
document.querySelector('header').addEventListener('mousedown', e => { if(e.button === 0 && e.detail === 1) invoke('drag'); });
document.querySelector('header').addEventListener('dblclick', () => invoke('toggle'));