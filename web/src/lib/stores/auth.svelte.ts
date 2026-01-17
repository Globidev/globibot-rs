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

  async login(code: string): Promise<DiscordMember> {
    this.isLoggingIn = true;
    try {
      const response = await fetch('/api/discord/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ code })
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

export interface DiscordMember {
  username: string;
  avatar_url: string | null;
}
