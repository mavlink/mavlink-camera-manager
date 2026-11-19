<template>
  <span class="info-anchor">
    <button
      ref="button"
      type="button"
      class="info-button"
      :class="{ 'info-button-text': !isCompact }"
      :aria-expanded="open"
      :aria-label="buttonAriaLabel"
      @click.stop="toggle"
    >
      {{ label }}
    </button>
    <Teleport to="body">
      <div
        v-if="open"
        ref="card"
        class="info-card"
        :class="{ 'info-card-wide': wide }"
        role="dialog"
        :style="cardStyle"
        @click.stop
      >
        <p v-if="description">{{ description }}</p>
        <slot />
      </div>
    </Teleport>
  </span>
</template>

<script lang="ts">
import { defineComponent } from "vue";

const VIEWPORT_MARGIN = 12;

export default defineComponent({
  name: "InfoCard",
  props: {
    description: {
      type: String,
      default: "",
    },
    label: {
      type: String,
      default: "?",
    },
    ariaLabel: {
      type: String,
      default: "",
    },
    wide: {
      type: Boolean,
      default: false,
    },
  },
  data() {
    return {
      open: false,
      cardStyle: {
        top: "0px",
        left: "0px",
      },
    };
  },
  computed: {
    isCompact(): boolean {
      return this.label === "?";
    },
    buttonAriaLabel(): string {
      if (this.ariaLabel) {
        return this.ariaLabel;
      }
      return this.isCompact ? "Show description" : this.label;
    },
  },
  watch: {
    description() {
      this.close();
    },
    label() {
      this.close();
    },
  },
  beforeUnmount() {
    this.removeListeners();
  },
  methods: {
    toggle() {
      if (this.open) {
        this.close();
      } else {
        this.open = true;
        this.$nextTick(() => {
          this.updatePosition();
          requestAnimationFrame(() => this.updatePosition());
          document.addEventListener(
            "pointerdown",
            this.onDocumentPointerDown,
            true
          );
          document.addEventListener("keydown", this.onDocumentKeyDown);
          window.addEventListener("resize", this.updatePosition);
          window.addEventListener("scroll", this.updatePosition, true);
        });
      }
    },
    close() {
      this.open = false;
      this.removeListeners();
    },
    removeListeners() {
      document.removeEventListener(
        "pointerdown",
        this.onDocumentPointerDown,
        true
      );
      document.removeEventListener("keydown", this.onDocumentKeyDown);
      window.removeEventListener("resize", this.updatePosition);
      window.removeEventListener("scroll", this.updatePosition, true);
    },
    updatePosition() {
      const button = this.$refs.button;
      const card = this.$refs.card;
      if (!(button instanceof HTMLElement) || !(card instanceof HTMLElement)) {
        return;
      }
      const button_box = button.getBoundingClientRect();
      const card_width = card.offsetWidth;
      const card_height = card.offsetHeight;
      const max_left = Math.max(
        VIEWPORT_MARGIN,
        window.innerWidth - VIEWPORT_MARGIN - card_width
      );
      const max_top = Math.max(
        VIEWPORT_MARGIN,
        window.innerHeight - VIEWPORT_MARGIN - card_height
      );
      let left = button_box.right - card_width;
      if (left < VIEWPORT_MARGIN) {
        left = VIEWPORT_MARGIN;
      } else if (left > max_left) {
        left = max_left;
      }
      let top = button_box.bottom + 8;
      if (top > max_top) {
        top = button_box.top - card_height - 8;
      }
      if (top < VIEWPORT_MARGIN) {
        top = VIEWPORT_MARGIN;
      } else if (top > max_top) {
        top = max_top;
      }
      this.cardStyle = {
        top: `${top}px`,
        left: `${left}px`,
      };
    },
    onDocumentPointerDown(event: PointerEvent) {
      const target = event.target as Node | null;
      const button = this.$refs.button;
      const card = this.$refs.card;
      if (button instanceof Node && target && button.contains(target)) {
        return;
      }
      if (card instanceof Node && target && card.contains(target)) {
        return;
      }
      this.close();
    },
    onDocumentKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        this.close();
      }
    },
  },
});
</script>

<style scoped>
.info-anchor {
  position: relative;
  display: inline-block;
  margin-left: 0.35em;
  vertical-align: middle;
  font-weight: normal;
}
.info-button {
  width: 1.25em;
  height: 1.25em;
  padding: 0;
  border: 1px solid #888;
  border-radius: 50%;
  background: #f4f4f4;
  color: #333;
  font-size: 0.8em;
  line-height: 1;
  cursor: pointer;
}
.info-button-text {
  width: auto;
  height: auto;
  padding: 0.15em 0.5em;
  border-radius: 3px;
  font-size: 0.75em;
  line-height: 1.3;
}
.info-button:hover,
.info-button[aria-expanded="true"] {
  background: #e8e8e8;
}
.info-card {
  position: fixed;
  z-index: 1000;
  box-sizing: border-box;
  width: 22em;
  max-width: calc(100vw - 24px);
  max-height: calc(100vh - 24px);
  padding: 1.1em 1.25em;
  overflow: auto;
  border: 1px solid #bbb;
  border-radius: 4px;
  background: #fff;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.18);
  color: #333;
  font-size: 0.85rem;
  font-weight: normal;
  line-height: 1.45;
  white-space: normal;
  overflow-wrap: break-word;
  word-break: break-word;
}
.info-card-wide {
  width: 28em;
}
.info-card p {
  margin: 0;
}
.info-card :deep(dl) {
  display: grid;
  grid-template-columns: 10em 1fr;
  gap: 0.35em 0.8em;
  margin: 0;
}
.info-card :deep(dt) {
  color: #555;
  font-weight: 600;
}
.info-card :deep(dd) {
  margin: 0;
  overflow-wrap: anywhere;
}
.info-card :deep(a) {
  color: #06c;
}
</style>
