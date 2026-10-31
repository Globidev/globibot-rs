class AuthStore {
  user = $state<DiscordMember | null>(null);
  initialized = $state(false);
  isLoggingIn = $state(false);
  logginError = $state<string | null>(null);

  async loadUserProfile() {
    try {
      const res = await fetch('/api/discord/profile');
      if (res.ok) {
        this.user = await res.json();
      } else {
        this.user = null;
      }
    } catch (err) {
      console.error('Failed to load user profile:', err);
      this.user = null;
    } finally {
      this.initialized = true;
    }
  }

  async login(code: string, state: string): Promise<DiscordMember> {
    this.isLoggingIn = true;
    try {
      const response = await fetch('/api/discord/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code, state })
      });
      if (!response.ok) {
        throw new Error(await response.text());
      }
      const member: DiscordMember = await response.json();
      this.user = member;
      return member;
    } catch (err) {
      this.logginError = (err as Error).message;
      throw err;
    } finally {
      this.isLoggingIn = false;
    }
  }

  async logout() {
    await fetch('/api/discord/logout', { method: 'POST' });
    this.user = null;
  }
}

export const authStore = new AuthStore();

const RETURN_PATH_KEY = 'login_return_path';

/** Remembers the current page, to come back to it after logging in with Discord */
export function rememberReturnPath() {
  const { pathname, search, hash } = window.location;
  sessionStorage.setItem(RETURN_PATH_KEY, pathname + search + hash);
}

/** The page to go back to after logging in, once. Only paths of this site */
export function takeReturnPath(): string {
  const path = sessionStorage.getItem(RETURN_PATH_KEY);
  sessionStorage.removeItem(RETURN_PATH_KEY);
  const isLocal = path?.startsWith('/') && !path.startsWith('//') && !path.startsWith('/oauth');
  return isLocal ? path! : '/';
}

export interface DiscordMember {
  user_id: string;
  username: string;
  avatar_url: string | null;
}
