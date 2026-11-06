/**
 * The message to show for a failed response: the API's own text, or a short one when the body is a
 * proxy's HTML error page (e.g. Cloudflare's 502 while a plugin restarts).
 */
export async function errorText(res: Response): Promise<string> {
  if ([502, 503, 504].includes(res.status)) {
    return 'The server is restarting or unreachable. Try again in a few seconds.';
  }
  const text = await res.text();
  if (res.headers.get('content-type')?.includes('text/html') || !text.trim()) {
    return `Request failed (${res.status})`;
  }
  return text;
}
