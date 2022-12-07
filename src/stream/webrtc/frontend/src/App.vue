<script setup lang="ts">
import "webrtc-adapter";

import { Manager } from "@/manager";

import { reactive, ref } from "vue";

// Hack to update object fields when they are not being tracked by Vue reactivity proxy.
const componentKey = ref(0);
function update() {
  componentKey.value += 1;
}
setInterval(update, 1000);

const ip = window.location.hostname.toString();

// eslint-disable-next-line no-undef
const rtc_configuration: RTCConfiguration = {
  bundlePolicy: "max-bundle",
  iceServers: [
    {
      urls: `turn:${ip}:3478`,
      username: "user",
      credential: "pwd",
      credentialType: "password",
    },
    {
      urls: `stun:${ip}:3478`,
    },
  ],
};

const manager = reactive(new Manager(ip, 6021, rtc_configuration));
</script>

<template>
  <header :key="componentKey">
    <h1>WebRTC Development UI</h1>
    <div>
      <p>Status: {{ manager.status }}</p>
      <p>Consumers: {{ manager.consumers.size }}</p>
    </div>
    <div>
      <button type="button" v-on:click="manager.addConsumer()">
        Add consumer
      </button>
      <button type="button" v-on:click="manager.removeAllConsumers()">
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
        <p>Consumer {{ consumer.id }}</p>
        <div>
          <p>Status: {{ consumer.status }}</p>
          <p>Producers: {{ consumer.streams.size }}</p>
          <p>Sessions: {{ consumer.sessions.size }}</p>
        </div>
        <button type="button" v-on:click="manager.removeConsumer(consumer.id)">
          Remove consumer
        </button>
        <button
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
          <p>Producer {{ stream.id }}</p>
          <p>
            Stream: "{{ stream.name }}" - {{ stream.width }}x{{
              stream.height
            }}@{{ stream.interval }}
            -
            {{ stream.encode }}
          </p>
          <button
            type="button"
            v-on:click="manager.addSession(consumer.id, stream.id)"
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
          <p>Session {{ session.id }}</p>
          <div>
            <p>
              Stream: "{{ session.stream.name }}" -
              {{ session.stream.width }}x{{ session.stream.height }}@{{
                session.stream.interval
              }}
              - {{ session.stream.encode }}
            </p>
            <p>Status: {{ session.status }}</p>
          </div>
          <button
            type="button"
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
