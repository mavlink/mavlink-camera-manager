import type { Stream } from "@/signalling_protocol";

import type { Signaller } from "@/signaller";

type on_close_callback = (session_id: string, reason: string) => void;

export class Session {
  public id: string;
  public consumer_id: string;
  public stream: Stream;
  public status: string;
  public allowedIps: string[];
  public allowedProtocols: string[];
  public jitterBufferTarget: number;
  public contentHint: string;
  public media_element: HTMLMediaElement | undefined;
  public signaller: Signaller;
  public peer_connection: RTCPeerConnection;
  public on_close?: on_close_callback;

  constructor(
    session_id: string,
    consumer_id: string,
    stream: Stream,
    signaller: Signaller,
    bundlePolicy: RTCBundlePolicy,
    iceServers: RTCIceServer[],
    allowedIps: string[],
    allowedProtocols: string[],
    jitterBufferTarget: number | null,
    contentHint: string,
    on_close?: on_close_callback
  ) {
    this.id = session_id;
    this.consumer_id = consumer_id;
    this.stream = stream;
    this.on_close = on_close;
    this.status = "";
    this.signaller = signaller;
    this.allowedIps = allowedIps;
    this.allowedProtocols = allowedProtocols;
    this.jitterBufferTarget = jitterBufferTarget;
    this.contentHint = contentHint;

    console.log("allowedIps:", this.allowedIps);

    const rtcConfiguration: RTCConfiguration = {
      bundlePolicy: bundlePolicy,
      iceServers: iceServers,
    };

    this.peer_connection = this.createRTCPeerConnection(rtcConfiguration);

    this.updateStatus("Creating Session...");

    // TODO: Fix this, should not end when WebSocket is closed!!!
    this.signaller.ws.addEventListener("close", () => {
      const reason = "Signalling closed.";
      console.debug(reason);
      this.updateStatus(reason);
      this.on_close?.(this.id, reason);
    });
  }

  public updateStatus(status: string): void {
    this.status = status;
  }

  private getMediaElement(): HTMLVideoElement | undefined {
    const video_id = `#video-${this.id}`;
    const media_element: HTMLVideoElement | null =
      document.querySelector(video_id);
    if (media_element === null) {
      console.error(`Video element ${video_id} not found`);
      return;
    }

    console.debug(`Video element ${video_id} found`);
    return media_element;
  }

  private createRTCPeerConnection(
    configuration: RTCConfiguration
  ): RTCPeerConnection {
    console.debug("Creating RTCPeerConnection");

    const peer_connection = new RTCPeerConnection(configuration);
    peer_connection.addTransceiver("video", {
      direction: "recvonly",
    });

    peer_connection.addEventListener("track", this.onTrackAdded.bind(this));

    peer_connection.addEventListener(
      "icecandidate",
      this.onIceCandidate.bind(this)
    );

    peer_connection.addEventListener(
      "iceconnectionstatechange",
      this.onIceConnectionStateChange.bind(this)
    );
    peer_connection.addEventListener(
      "connectionstatechange",
      this.onConnectionStateChange.bind(this)
    );
    peer_connection.addEventListener(
      "signalingstatechange",
      this.onSignalingStateChange.bind(this)
    );
    peer_connection.addEventListener(
      "icegatheringstatechange",
      this.onIceGatheringStateChange.bind(this)
    );

    return peer_connection;
  }

  public onIncomingSDP(description: RTCSessionDescription): void {
    this.peer_connection
      .setRemoteDescription(description)
      .then(() => {
        console.debug(
          `Remote description set to ${JSON.stringify(description, null, 4)}`
        );
        this.onRemoteDescriptionSet();
      })
      .catch((reason) =>
        console.error(
          `Failed setting remote description ${description}. Reason: ${reason}`
        )
      );
  }

  private onRemoteDescriptionSet(): void {
    this.peer_connection
      .createAnswer()
      .then((description: RTCSessionDescriptionInit) => {
        console.debug(
          `SDP Answer created as: ${JSON.stringify(description, null, 4)}`
        );
        this.onAnswerCreated(description);
      })
      .catch((reason) =>
        console.error(`Failed creating description answer. Reason: ${reason}`)
      );
  }

  private onAnswerCreated(description: RTCSessionDescriptionInit): void {
    this.peer_connection
      .setLocalDescription(description)
      .then(() => {
        console.debug(
          `Local description set as${JSON.stringify(description, null, 4)}`
        );
        this.onLocalDescriptionSet();
      })
      .catch(function (reason) {
        console.error(`Failed setting local description. Reason: ${reason}`);
      });
  }

  private onLocalDescriptionSet(): void {
    if (this.peer_connection.localDescription === null) {
      return;
    }

    this.signaller.sendMediaNegotiation(
      this.id,
      this.consumer_id,
      this.stream.id,
      this.peer_connection.localDescription
    );
  }

  public onIncomingICE(candidate: RTCIceCandidateInit): void {
    const c = candidate.candidate.toLowerCase();

    const ip_disallowed =
      this.allowedIps.length > 0 &&
      this.allowedIps[0].length > 0 &&
      this.allowedIps.find((ip) => {
        return c.includes(ip);
      }) === undefined;

    const protocol_disallowed =
      this.allowedProtocols.find((protocol) => {
        return c.includes(protocol);
      }) === undefined;

    if (ip_disallowed || protocol_disallowed) {
      console.log("Skipping remote ICE candidate:", c);
      return;
    }

    this.peer_connection
      .addIceCandidate(candidate)
      .then(() =>
        console.debug(
          `Remote ICE candidate added: ${JSON.stringify(candidate, null, 4)}`
        )
      )
      .catch((reason) =>
        console.error(
          `Failed adding ICE candidate ${candidate}. Reason: ${reason}`
        )
      );
  }

  private onIceCandidate(event: RTCPeerConnectionIceEventInit): void {
    if (!event.candidate) {
      // TODO: Add support for empty candidate, meaning ICE Gathering Completed.
      return;
    }

    this.signaller.sendIceNegotiation(
      this.id,
      this.consumer_id,
      this.stream.id,
      event.candidate
    );
  }

  private onTrackAdded(event: RTCTrackEvent): void {
    let id: number | undefined = undefined;
    id = window.setInterval(() => {
      if (this.signaller.ws.readyState !== this.signaller.ws.OPEN) {
        clearInterval(id);
      }

      this.media_element = this.getMediaElement();
      if (this.media_element === undefined) {
        return;
      }

      this.peer_connection
        .getReceivers()
        .forEach((receiver: RTCRtpReceiver) => {
          if (receiver.track.kind !== "video") {
            return;
          }

          console.debug(
            `RTCRtpReceiver jitterBufferTarget attribute set from ${
              (receiver as any).jitterBufferTarget
            } to ${this.jitterBufferTarget}`
          );
          (receiver as any).jitterBufferTarget = this.jitterBufferTarget; // in milliseconds (DOMHighResTimeStamp)

          let playoutDelayHint = null;
          if (this.jitterBufferTarget !== null) {
            playoutDelayHint = this.jitterBufferTarget / 1000; // in seconds, legacy Chrome API
          }
          console.debug(
            `RTCRtpReceiver playoutDelayHint attribute set from ${
              (receiver as any).playoutDelayHint
            } to ${playoutDelayHint}`
          );
          (receiver as any).playoutDelayHint = playoutDelayHint;
        });

      event.streams.forEach((stream) => {
        stream.getVideoTracks().forEach((video_track) => {
          console.log(
            `"MediaStreamTrack contentHint attribute set from ${video_track.contentHint} to ${this.contentHint}"`
          );
          video_track.contentHint = this.contentHint;
        });
      });

      const [remoteStream] = event.streams;
      this.media_element.srcObject = remoteStream;
      this.media_element.play();

      this.updateStatus("Playing");

      clearInterval(id);
    }, 1000);
  }

  private onIceConnectionStateChange(): void {
    switch (this.peer_connection.iceConnectionState) {
      case "closed":
      case "failed":
      case "disconnected":
        this.end();
        break;
    }
  }

  private onConnectionStateChange(): void {
    switch (this.peer_connection.connectionState) {
      case "closed":
      case "failed":
      case "disconnected":
        this.end();
        break;
    }
  }

  private onSignalingStateChange(): void {
    switch (this.peer_connection.signalingState) {
      case "closed":
        this.end();
        break;
    }
  }

  private onIceGatheringStateChange(): void {
    switch (this.peer_connection.iceGatheringState) {
      case "complete":
        console.debug(`ICE gathering completed for session ${this.id}`);
        break;
    }
  }

  public end() {
    this.peer_connection.close();

    this.peer_connection.removeEventListener(
      "track",
      this.onTrackAdded.bind(this)
    );
    this.peer_connection.removeEventListener(
      "iceconnectionstatechange",
      this.onIceConnectionStateChange
    );
    this.peer_connection.removeEventListener(
      "connectionstatechange",
      this.onConnectionStateChange
    );
    this.peer_connection.removeEventListener(
      "signalingstatechange",
      this.onSignalingStateChange
    );
    this.peer_connection.removeEventListener(
      "icegatheringstatechange",
      this.onIceGatheringStateChange
    );

    console.debug(`Session ${this.id} ended.`);
  }
}
