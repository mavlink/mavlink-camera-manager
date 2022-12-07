import type {
  Message,
  Answer,
  Stream,
  Negotiation,
} from "@/signalling_protocol";

type on_status_change_callback = (status: string) => void;
type on_available_streams_callback = (streams: Array<Stream>) => void;
type on_consumer_id_received_callback = (consumer_id: string) => void;
type on_session_id_received_callback = (session_id: string) => void;
type on_session_end_callback = (session_id: string, reason: string) => void;
type on_ice_negotiation_callback = (candidate: RTCIceCandidateInit) => void;
type on_media_negotiation_callback = (
  description: RTCSessionDescription
) => void;

export class Signaller {
  public ws: WebSocket;
  public on_status_change?: on_status_change_callback;
  private should_reconnect: boolean;

  constructor(
    url: URL,
    should_reconnect: boolean,
    on_status_change?: on_status_change_callback
  ) {
    this.on_status_change = on_status_change;
    this.should_reconnect = should_reconnect;

    const status = `Connecting to signalling server on ${url}`;
    console.debug(status);
    this.on_status_change?.(status);

    this.ws = this.connect(url.toString());
  }

  public requestConsumerId(
    on_consumer_id_received: on_consumer_id_received_callback,
    on_status_changed?: on_status_change_callback
  ): void {
    const ws = this.ws;
    this.ws.addEventListener(
      "message",
      function consumer_id_listener(ev: MessageEvent): void {
        try {
          const message: Message = JSON.parse(ev.data);
          if (message.type !== "answer") {
            return;
          }

          const answer: Answer = message.content;
          if (answer.type !== "peerId") {
            return;
          }

          const consumer_id: string = answer.content.id;
          on_status_changed?.(`Consumer Id arrived: ${consumer_id}`);
          on_consumer_id_received(consumer_id);
          ws.removeEventListener("message", consumer_id_listener);
        } catch (error) {
          const error_msg = `Failed receiving PeerId Answer Message. Error: ${error}. Data: ${ev.data}`;
          console.error(error_msg);
          on_status_changed?.(error_msg);
          return;
        }
      }
    );

    const message: Message = {
      type: "question",
      content: {
        type: "peerId",
      },
    };

    try {
      this.ws.send(JSON.stringify(message));
      on_status_changed?.("Consumer Id requested, waiting answer...");
    } catch (reason) {
      const error = `Failed requesting peer id. Reason: ${reason}`;
      console.error(error);
      on_status_changed?.(error);
    }
  }

  public requestStreams(on_status_changed?: on_status_change_callback): void {
    const message: Message = {
      type: "question",
      content: {
        type: "availableStreams",
      },
    };

    try {
      this.ws.send(JSON.stringify(message));
      on_status_changed?.("StreamsAvailable requested");
    } catch (error) {
      const error_msg = `Failed requesting available streams. Reason: ${error}`;
      console.error(error_msg);
      on_status_changed?.(error_msg);
    }
  }

  public requestSessionId(
    consumer_id: string,
    producer_id: string,
    on_session_id_received: on_session_id_received_callback,
    on_status_changed?: on_status_change_callback
  ): void {
    const signaller = this;
    signaller.ws.addEventListener(
      "message",
      function session_start_listener(ev: MessageEvent): void {
        try {
          const session_id = signaller.parseSessionStartAnswer(ev);
          if (session_id === undefined) {
            return;
          }

          // Only remove after getting the right message
          signaller.ws.removeEventListener("message", session_start_listener);

          on_status_changed?.(`Session Id arrived: ${session_id}`);
          on_session_id_received(session_id);
        } catch (error) {
          const error_msg = `Failed receiving StartSession Answer Message. Error: ${error}. Data: ${ev.data}`;
          console.error(error_msg);
          on_status_changed?.(error_msg);
          return;
        }
      }
    );

    const message: Message = {
      type: "question",
      content: {
        type: "startSession",
        content: {
          consumer_id,
          producer_id,
        },
      },
    };

    try {
      this.ws.send(JSON.stringify(message));
      on_status_changed?.("Session Id requested, waiting answer...");
    } catch (reason) {
      const error = `Failed requesting Session Id. Reason: ${reason}`;
      console.error(error);
      on_status_changed?.(error);
    }
  }

  sendIceNegotiation(
    session_id: string,
    consumer_id: string,
    producer_id: string,
    ice: RTCIceCandidate,
    on_status_changed?: on_status_change_callback
  ) {
    const message: Message = {
      type: "negotiation",
      content: {
        type: "iceNegotiation",
        content: {
          session_id,
          consumer_id,
          producer_id,
          ice: ice.toJSON(),
        },
      },
    };

    console.debug(`Sending ICE answer: ${JSON.stringify(message, null, 4)}`);

    try {
      this.ws.send(JSON.stringify(message));
      on_status_changed?.("ICE Candidate sent");
    } catch (error) {
      const error_msg = `Failed sending ICE Candidate. Reason: ${error}`;
      console.error(error_msg);
      on_status_changed?.(error_msg);
    }
  }

  sendMediaNegotiation(
    session_id: string,
    consumer_id: string,
    producer_id: string,
    sdp: RTCSessionDescription,
    on_status_changed?: on_status_change_callback
  ) {
    const message: Message = {
      type: "negotiation",
      content: {
        type: "mediaNegotiation",
        content: {
          session_id,
          consumer_id,
          producer_id,
          sdp: sdp.toJSON(),
        },
      },
    };

    try {
      this.ws.send(JSON.stringify(message));
      on_status_changed?.("ICE Candidate sent");
    } catch (error) {
      const error_msg = `Failed sending SDP. Reason: ${error}`;
      console.error(error_msg);
      on_status_changed?.(error_msg);
    }
  }

  parseSessionStartAnswer(ev: MessageEvent): string | undefined {
    const message: Message = JSON.parse(ev.data);
    if (message.type !== "answer") {
      return;
    }

    const answer: Answer = message.content;
    if (answer.type !== "startSession") {
      return;
    }

    return answer.content.session_id;
  }

  public parseEndSessionQuestion(
    consumer_id: string,
    producer_id: string,
    session_id: string,
    on_session_end: on_session_end_callback,
    on_status_changed?: on_status_change_callback
  ): void {
    this.ws.addEventListener("message", (ev: MessageEvent): void => {
      try {
        const message: Message = JSON.parse(ev.data);
        if (message.type !== "question") {
          return;
        }

        const question = message.content;
        if (question.type !== "endSession") {
          return;
        }

        const end_session_question = question.content;
        if (
          end_session_question.consumer_id !== consumer_id ||
          end_session_question.producer_id !== producer_id ||
          end_session_question.session_id !== session_id
        ) {
          return;
        }

        const reason = end_session_question.reason;
        on_status_changed?.("EndSession arrived");
        on_session_end?.(session_id, reason);
      } catch (error) {
        const error_msg = `Failed parsing received Message. Error: ${error}. Data: ${ev.data}`;
        console.error(error_msg);
        on_status_changed?.(error_msg);
        return;
      }
    });
  }

  public parseNegotiation(
    consumer_id: string,
    producer_id: string,
    session_id: string,
    on_ice_negotiation?: on_ice_negotiation_callback,
    on_media_negotiation?: on_media_negotiation_callback,
    on_status_changed?: on_status_change_callback
  ): void {
    this.ws.addEventListener("message", (ev: MessageEvent) => {
      console.debug(`Message received: ${ev.data}`);
      try {
        const message: Message = JSON.parse(ev.data);

        if (message.type !== "negotiation") {
          return;
        }
        const negotiation: Negotiation = message.content;

        if (
          negotiation.content.consumer_id !== consumer_id ||
          negotiation.content.producer_id !== producer_id ||
          negotiation.content.session_id !== session_id
        ) {
          return;
        }

        switch (negotiation.type) {
          case "iceNegotiation":
            on_status_changed?.("iceNegotiation arrived");
            on_ice_negotiation?.(negotiation.content.ice);
            break;

          case "mediaNegotiation":
            on_status_changed?.("mediaNegotiation arrived");
            on_media_negotiation?.(negotiation.content.sdp);
            break;
        }
      } catch (error) {
        const error_msg = `Failed parsing received Message. Error: ${error}. Data: ${ev.data}`;
        console.error(error_msg);
        on_status_changed?.(error_msg);
        return;
      }
    });
  }

  public parseAvailableStreamsAnswer(
    on_available_streams: on_available_streams_callback,
    on_status_changed?: on_status_change_callback
  ): void {
    this.ws.addEventListener("message", (ev: MessageEvent): void => {
      try {
        const message: Message = JSON.parse(ev.data);
        if (message.type !== "answer") {
          return;
        }

        const answer: Answer = message.content;
        if (answer.type !== "availableStreams") {
          return;
        }

        const streams: Array<Stream> = answer.content;
        on_status_changed?.("Available Streams arrived");
        on_available_streams?.(streams);
      } catch (error) {
        const error_msg = `Failed parsing received Message. Error: ${error}. Data: ${ev.data}`;
        console.error(error_msg);
        on_status_changed?.(error_msg);
        return;
      }
    });
  }

  public end(id: string) {
    this.should_reconnect = false;
    this.ws.removeEventListener("open", (ev: Event) => this.onOpen(ev));
    this.ws.removeEventListener("error", (ev: Event) => this.onError(ev));
    // this.ws.removeEventListener("message", (ev: MessageEvent) =>
    //   this.onMessage(ev)
    // );
    this.ws.removeEventListener("close", (ev: CloseEvent) => this.onClose(ev));

    if (this.ws.readyState !== (this.ws.CLOSED | this.ws.CLOSING)) {
      this.ws.close();
      console.debug(`Closing WebSocket from id ${id}`);
    }
  }

  private connect(url: string) {
    const ws = new WebSocket(url);

    ws.addEventListener("open", (ev: Event) => this.onOpen(ev));
    ws.addEventListener("error", (ev: Event) => this.onError(ev));
    // ws.addEventListener("message", (ev: MessageEvent) => this.onMessage(ev));
    ws.addEventListener("close", (ev: CloseEvent) => this.onClose(ev));

    return ws;
  }

  private reconnect(url: string) {
    const status = `Reconnecting to signalling server on ${url}`;
    console.debug(status);
    this.on_status_change?.(status);

    // Avoid multiple reconnections
    if (this.ws.readyState === this.ws.CONNECTING) {
      return;
    }

    this.end("");

    this.ws = this.connect(url);
  }

  private onOpen(event: Event) {
    console.debug("onOpen called. Event:", event);

    const status = `Signaller Connected`;
    console.debug(status);
    this.on_status_change?.(status);
  }

  private onClose(event: CloseEvent) {
    console.debug("onClose called. Event:", event);

    const status = `Signaller connection closed`;
    console.debug(status);
    this.on_status_change?.(status);

    if (this.should_reconnect) {
      setTimeout(() => this.reconnect(this.ws.url), 1000);
    }
  }

  private onError(event: Event) {
    console.debug("onError called. Event:", event);

    const status = `Signaller connection Error`;
    console.debug(status);
    this.on_status_change?.(status);

    const url = this.ws.url;

    if (this.should_reconnect) {
      setTimeout(() => this.reconnect(url), 1000);
    }
  }

  private onMessage(event: MessageEvent) {
    console.debug("Signaller onMessage called. Event:", event);
  }
}
