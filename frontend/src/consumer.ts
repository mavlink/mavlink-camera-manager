import { Result } from "@badrap/result";

import type { Stream } from "@/signalling_protocol";
import type { Session } from "@/session";
import type { Signaller } from "@/signaller";

export class Consumer {
  public id: string;
  public status: string;
  public sessions: Map<string, Session>;
  public streams: Map<string, Stream>;
  public signaller: Signaller;
  public signallerStatus: string;

  constructor(id: string, signaller: Signaller) {
    this.id = id;
    this.sessions = new Map<string, Session>();
    this.streams = new Map<string, Stream>();
    this.status = "";
    this.signaller = signaller;
    this.signallerStatus = "";
  }

  public updateStatus(status: string) {
    console.debug(`Status updated to ${status}`);
    const time = new Date().toLocaleTimeString("en-US", { hour12: false });
    this.status = `[${time}]: ${status}`;
  }

  public updateSignallerStatus(status: string) {
    console.debug(`Signaller Status updated to ${status}`);
    const time = new Date().toLocaleTimeString("en-US", { hour12: false });
    this.status = `[${time}]: ${status}`;
  }

  public updateStreams(streams: Array<Stream>): void {
    this.streams.clear();
    streams.forEach((stream: Stream): void => {
      this.streams.set(stream.id, stream);
    });
  }

  public addSession(session: Session): void {
    this.sessions.set(session.id, session);
  }

  public removeSession(session_id: string): Result<void> {
    const session = this.sessions.get(session_id);
    if (session === undefined) {
      const error = `Failed to find session ${session_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    session.end();
    this.sessions.delete(session_id);
    return Result.ok(undefined as void);
  }

  public removeAllSessions(): Result<void> {
    this.sessions.forEach((session: Session): void => session.end());
    this.sessions.clear();
    console.debug(
      `All sessions from consumer ${
        this.id
      } removed. Current sessions: ${JSON.stringify(this.sessions, null, 4)}`
    );
    return Result.ok(undefined as void);
  }

  public end(): void {
    this.removeAllSessions();
    this.signaller.end(`consumer-${this.id}`);
  }
}
