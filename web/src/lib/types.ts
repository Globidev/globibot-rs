export interface Plugin {
  name: string;

  has_rpc: boolean;
  has_events: boolean;
  has_web_api: boolean;

  startup_ts: number;
}
