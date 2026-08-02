export const CORE_API_BASE = "/api/v1";

export function createClient(getToken) {
  function requestHeaders(options) {
    const headers = new Headers(options.headers || {});
    const token = getToken();
    if (token) headers.set("Authorization", `Bearer ${token}`);
    if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
    return headers;
  }

  async function webui(path, options = {}) {
    const response = await fetch(`/webui${path}`, { cache: "no-store", ...options, headers: requestHeaders(options) });
    const payload = await response.json();
    if (!response.ok) throw new Error(payload?.error?.message || `WebUI 请求失败 (${response.status})`);
    return payload.result;
  }

  async function api(path, options = {}) {
    const response = await fetch(`${CORE_API_BASE}${path}`, { cache: "no-store", ...options, headers: requestHeaders(options) });
    const contentType = response.headers.get("content-type") || "";
    const payload = contentType.includes("application/json") ? await response.json() : null;
    if (!response.ok) {
      const coreError = payload?.error;
      const message = coreError ? `${coreError.message}${coreError.detail ? `：${coreError.detail}` : ""}` : `请求失败 (${response.status})`;
      const error = new Error(message);
      error.code = coreError?.code;
      error.status = response.status;
      throw error;
    }
    return payload?.result ?? payload;
  }

  return { api, webui };
}
