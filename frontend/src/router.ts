import { createRouter, createWebHistory } from "vue-router";

const router = createRouter({
  history: createWebHistory(),
  routes: [
    {
      path: "/",
      component: () => import("./home/HomeView.vue"),
    },
    {
      path: "/webrtc",
      component: () => import("./webrtc/WebRTCView.vue"),
    },
  ],
});

export default router;
