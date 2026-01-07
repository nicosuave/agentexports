chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (!message || message.type !== "agentexport-fetch") return undefined;

  fetch(message.url)
    .then(async (response) => {
      const buffer = await response.arrayBuffer();
      const bytes = new Uint8Array(buffer);
      let binary = "";
      const chunkSize = 0x8000;
      for (let i = 0; i < bytes.length; i += chunkSize) {
        binary += String.fromCharCode(...bytes.subarray(i, i + chunkSize));
      }
      const bodyB64 = btoa(binary);
      sendResponse({ ok: response.ok, status: response.status, body_b64: bodyB64 });
    })
    .catch((err) => {
      sendResponse({ ok: false, error: err && err.message ? err.message : String(err) });
    });

  return true;
});
