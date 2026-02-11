import { Result } from "@badrap/result";

import { Consumer } from "@/consumer";
import { Signaller } from "@/signaller";
import { Session } from "@/session";

export class Manager {
  public status: string;
  public consumers: Map<String, Consumer> = new Map();

  public updateStatus(status: string): void {
    const time = new Date().toLocaleTimeString("en-US", { hour12: false });
    this.status = `[${time}]: ${status}`;
  }

  public addConsumer(signaller_ip: string, signaller_port: number): void {
    const websocket_address = new URL(`ws://${signaller_ip}:${signaller_port}`);

    // Each consumer has its own signaller, which is shared with all its Sessions.
    const signaller = new Signaller(websocket_address, true, null);

    signaller.ws.addEventListener(
      "open",
      (): void => {
        signaller.requestConsumerId((consumer_id: string): void => {
          const consumer = new Consumer(consumer_id, signaller);
          // Updates its list of streams whenever it receives available streams
          signaller.parseAvailableStreamsAnswer(
            consumer.updateStreams.bind(consumer)
          );
          // Updates its status whenever signalling got a new status
          consumer.signaller.on_status_change =
            consumer.updateSignallerStatus.bind(consumer);

          this.consumers.set(consumer_id, consumer);

          signaller.requestStreams();

          // Regularly asks for available streams, which will trigger the consumer "on_available_streams" callback
          let handler_id: number | undefined = undefined;
          handler_id = window.setInterval(() => {
            if (signaller.ws.readyState !== signaller.ws.OPEN) {
              clearInterval(handler_id);
            }

            signaller.requestStreams();
          }, 1000);
        }, this.updateStatus.bind(this));
      },
      { once: true }
    );
  }

  public removeConsumer(consumer_id: string): Result<void> {
    const consumer = this.consumers.get(consumer_id);
    if (consumer === undefined) {
      const error = `Failed to find consumer ${consumer_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    consumer.end();
    this.consumers.delete(consumer_id);
    console.debug(
      `Consumer ${consumer_id} removed. Current consumers: ${JSON.stringify(
        this.consumers.values(),
        null,
        4
      )}`
    );
    return Result.ok(undefined as void);
  }

  public removeAllConsumers(): Result<void> {
    this.consumers.forEach((consumer: Consumer): void => consumer.end());
    this.consumers.clear();
    console.debug("All consumers removed.");
    return Result.ok(undefined as void);
  }

  public addSession(
    consumer_id: string,
    producer_id: string,
    bundlePolicy: RTCBundlePolicy,
    iceServers: RTCIceServer[],
    allowedIps: string[],
    allowedProtocols: string[],
    jitterBufferTarget: number | null,
    contentHint: string
  ): Result<void> {
    const consumer = this.consumers.get(consumer_id);
    if (consumer == undefined) {
      const error = `Failed to find consumer ${consumer_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    const stream = consumer.streams.get(producer_id);
    if (stream == undefined) {
      const error = `Failed to find stream ${producer_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    const signaller = consumer.signaller;
    signaller.requestSessionId(
      consumer_id,
      producer_id,
      (session_id: string): void => {
        const session = new Session(
          session_id,
          consumer_id,
          stream,
          signaller,
          bundlePolicy,
          iceServers,
          allowedIps,
          allowedProtocols,
          jitterBufferTarget,
          contentHint,
          (session_id: string): void => {
            consumer.removeSession(session_id);
          }
        );

        consumer.addSession(session);

        signaller.parseEndSessionQuestion(
          consumer_id,
          producer_id,
          session_id,
          (_session_id, reason) => {
            console.info(`Ending session ${session_id}. Reason: ${reason}`);
            this.removeSession(consumer_id, session_id);
          },
          session.updateStatus.bind(session)
        );

        signaller.parseNegotiation(
          consumer_id,
          producer_id,
          session_id,
          session.onIncomingICE.bind(session),
          session.onIncomingSDP.bind(session)
        );
      },
      consumer.updateStatus.bind(consumer)
    );

    return Result.ok(undefined as void);
  }

  public removeSession(consumer_id: string, session_id: string): Result<void> {
    const consumer = this.consumers.get(consumer_id);
    if (consumer == undefined) {
      const error = `Failed to find consumer ${consumer_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    return consumer.removeSession(session_id);
  }

  public removeAllSessions(consumer_id: string): Result<void> {
    const consumer = this.consumers.get(consumer_id);
    if (consumer == undefined) {
      const error = `Failed to find consumer ${consumer_id}`;
      console.log(error);
      return Result.err(Error(error));
    }

    return consumer.removeAllSessions();
  }
}
