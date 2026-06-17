/**
 * Application entry point and core service class.
 */

/** Configuration options for the App. */
export interface AppConfig {
  name: string;
  version: string;
  debug?: boolean;
}

/** Core application service. */
export class App {
  private readonly name: string;
  private readonly version: string;
  private debug: boolean;

  constructor(config: AppConfig) {
    this.name = config.name;
    this.version = config.version;
    this.debug = config.debug ?? false;
  }

  /** Return the application display string. */
  toString(): string {
    return `${this.name}@${this.version}`;
  }

  /** Enable or disable debug mode. */
  setDebug(value: boolean): void {
    this.debug = value;
  }

  /** Return true if debug mode is active. */
  isDebug(): boolean {
    return this.debug;
  }
}

/** Create an App instance with default settings. */
export function createApp(name: string): App {
  return new App({ name, version: "0.1.0" });
}

/** Utility: sleep for a given number of milliseconds. */
export async function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
