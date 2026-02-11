<script setup lang="ts">
import adapter from "webrtc-adapter";

import { Manager } from "@/manager";

import { reactive, ref, computed, watch, Ref } from "vue";

const signallerIp = ref(window.location.hostname.toString());
const signallerPort = ref(6021);

const iceAllowedProtocols = ref(["udp", "tcp"]);
const inner_iceAllowedIps: Ref<string[]> = ref([]);
// eslint-disable-next-line no-undef
const bundlePolicy = ref("max-bundle" as RTCBundlePolicy);
// eslint-disable-next-line no-undef
const iceServers: Ref<RTCIceServer[]> = ref([]);
const jitterBufferTarget: Ref<number | null> = ref(0);
const contentHint = ref("motion");

const iceAllowedIps = computed({
  get() {
    return inner_iceAllowedIps.value;
  },
  set(newValue) {
    const val = newValue
      .toString()
      .split("/;/ ?/,/ ?")
      .filter((ip: string) => ip.length > 0)
      .map((ip: string) => ip.toLowerCase().trim());
    console.debug(val);
    inner_iceAllowedIps.value = val;
  },
});

watch(jitterBufferTarget, (newValue: number | null) => {
  if (newValue === null || !Number.isFinite(newValue)) {
    jitterBufferTarget.value = null;
  }
  if (newValue < 0) {
    jitterBufferTarget.value = 0;
  }
  if (newValue > 4000) {
    jitterBufferTarget.value = 4000;
  }
});

console.log(
  `Using webrtc-adapter shim for browser ${adapter.browserDetails.browser}@{adapter.browserDetails.version}`
);

// Hack to update object fields when they are not being tracked by Vue reactivity proxy.
const componentKey = ref(0);
function update() {
  componentKey.value += 1;
}
setInterval(update, 200);

const manager = reactive(new Manager());
</script>

<template>
  <div :key="componentKey"></div>
  <h1>WebRTC Development UI</h1>
  <div>
    <p>
      Signaller Address:
      <input
        id="signallerIp"
        v-model="signallerIp"
        placeholder="ip, example: 192.168.0.2"
        type="text"
      />
      :
      <input
        id="signallerPort"
        v-model.number="signallerPort"
        placeholder="port, example: 6020"
        type="text"
      />
    </p>
  </div>
  <header>
    <div>
      <p id="status">Status: {{ manager.status }}</p>
      <p id="consumers">Consumers: {{ manager.consumers.size }}</p>
    </div>
    <div>
      <button
        id="add-consumer"
        type="button"
        v-on:click="manager.addConsumer(signallerIp, signallerPort)"
      >
        Add consumer
      </button>
      <button
        id="remove-all-consumers"
        type="button"
        v-on:click="manager.removeAllConsumers()"
      >
        Remove all consumers
      </button>
    </div>
  </header>
  <br />

  <main>
    <!-- Consumer -->
    <div
      class="consumer"
      v-for="consumer in manager.consumers.values()"
      :key="consumer.id"
      v-bind:id="'consumer-' + consumer.id"
    >
      <div class="consumer-header">
        <h2>Consumer</h2>
        <p id="consumer-id">Consumer {{ consumer.id }}</p>
        <div>
          <p id="consumer-status">Status: {{ consumer.status }}</p>
          <p id="consumer-signaller-status">
            Signaller Status: {{ consumer.signallerStatus }}
          </p>
          <p id="consumer-producers">Producers: {{ consumer.streams.size }}</p>
          <p id="consumer-sessions">Sessions: {{ consumer.sessions.size }}</p>
        </div>
        <button
          id="remove-consumer"
          type="button"
          v-on:click="manager.removeConsumer(consumer.id)"
        >
          Remove consumer
        </button>
        <button
          id="remove-all-sessions"
          type="button"
          v-on:click="manager.removeAllSessions(consumer.id)"
        >
          Remove all sessions
        </button>
      </div>

      <!-- Streams -->
      <div class="stream-container">
        <h3>Available Producers</h3>
        <div
          class="stream"
          v-for="stream in consumer.streams.values()"
          :key="stream.id"
          v-bind:id="'stream-' + stream.id"
        >
          <p id="producer-id">Producer {{ stream.id }}</p>
          <p>
            Stream: "{{ stream.name }}" - {{ stream.width }}x{{
              stream.height
            }}@{{ stream.interval }}
            -
            {{ stream.encode }}
          </p>
          <div>
            <p>
              ICE allowed IPs:
              <input
                id="iceAllowedIps"
                style="width: 400px"
                v-model="iceAllowedIps"
                placeholder="list of ips, e.g: 192.168.0.2 192.168.0.3; empty for all"
              />
            </p>
            <p>
              ICE allowed protocols:
              <a
                v-for="(option, index) in ['udp', 'tcp']"
                :key="index"
                style="margin-left: 15px"
              >
                <input
                  type="checkbox"
                  :id="option"
                  :value="option"
                  v-model="iceAllowedProtocols"
                />
                <label :for="option" style="margin-left: 5px">
                  {{ option }}</label
                >
              </a>
            </p>
            <p>
              Jitter Buffer Target (milliseconds):
              <input
                id="jitterBufferTarget"
                v-model.number="jitterBufferTarget"
                placeholder="null"
              />
            </p>
            <p>
              Content Hint:
              <a
                v-for="(option, index) in ['motion', 'detail', 'text']"
                :key="index"
                style="margin-left: 15px"
              >
                <input
                  type="radio"
                  :id="option"
                  :value="option"
                  v-model="contentHint"
                />
                <label :for="option" style="margin-left: 5px">{{
                  option
                }}</label>
              </a>
            </p>
          </div>
          <button
            id="add-session"
            type="button"
            @click="
              manager.addSession(
                consumer.id,
                stream.id,
                bundlePolicy,
                iceServers,
                iceAllowedIps,
                iceAllowedProtocols,
                jitterBufferTarget,
                contentHint
              )
            "
          >
            Add Session
          </button>
        </div>
        <hr />
      </div>

      <!-- Sessions -->
      <div class="session-container">
        <h3>Running Sessions</h3>
        <div
          class="session"
          v-for="session in consumer.sessions.values()"
          :key="session.id"
        >
          <p id="session-id">Session {{ session.id }}</p>
          <div>
            <p>
              Stream: "{{ session.stream.name }}" -
              {{ session.stream.width }}x{{ session.stream.height }}@{{
                session.stream.interval
              }}
              - {{ session.stream.encode }}
            </p>
            <p>
              Allowed ICE IPs:
              {{ session.allowedIps.length > 0 ? session.allowedIps : "(any)" }}
            </p>
            <p>Allowed ICE Protocols: {{ session.allowedProtocols }}</p>
            <p>
              Target Jitter Buffer (seconds):
              {{
                session.jitterBufferTarget !== null
                  ? session.jitterBufferTarget
                  : "(null)"
              }}
            </p>

            <p>Content Hint: "{{ session.contentHint }}"</p>
            <p id="session-status">Status: {{ session.status }}</p>
          </div>
          <button
            type="button"
            id="remove-session"
            v-on:click="manager.removeSession(consumer.id, session.id)"
          >
            Remove Session
          </button>

          <!-- Media Player -->
          <video controls width="320" v-bind:id="'video-' + session.id"></video>
        </div>
        <hr />
      </div>
    </div>
  </main>
</template>
