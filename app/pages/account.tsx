import { createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { Image, Text, View } from "@pocketjs/framework/components";
import { bgCls, pTxt } from "../core/theme";
import { cancelQrTexture, scheduleQrTexture } from "../core/qr";
import { logMsg } from "../core/media";
import { MenuRow } from "../components/rows";

/* 扫码登录页（子页）：二维码 + 说明 + 刷新按钮 + 记住登录。
 * 失效态 = 半透明遮罩 + 红「已过期」角标；还没拿到码时给一个虚线框提示，不留白块。 */

export type LoginSnapshot = {
  state: string;
  loggedIn: boolean;
  message: string;
  url: string;
};

const STATE_TEXT: Record<string, string> = {
  idle: "按 ○ 获取二维码",
  starting: "正在获取二维码…",
  waiting: "打开网易云音乐 APP 扫码",
  scanned: "已扫码，请在手机上确认",
  confirmed: "登录成功 SIGNED IN",
  expired: "二维码已失效",
  failed: "获取失败，请重试",
};

export function AccountPage(props: {
  snapshot: () => LoginSnapshot;
  remember: () => boolean;
  cursor: () => number;
  active: () => boolean;
}) {
  const expired = createMemo(() => props.snapshot().state === "expired");
  const caption = createMemo(() => {
    const snap = props.snapshot();
    if (snap.loggedIn) return "已登录 SIGNED IN";
    return STATE_TEXT[snap.state] ?? "打开网易云音乐 APP 扫码";
  });

  /* 只在 URL 真变了才重画贴图（每次轮询都传会把界面拖成幻灯片）。 */
  let lastQrUrl = "";
  const [qrTexKey, setQrTexKey] = createSignal("");
  createEffect(() => {
    const url = props.snapshot().url;
    if (!url || url === lastQrUrl) return;
    lastQrUrl = url;
    /*
     * 网络回调发生在 guest 帧里；二维码已经由 Rust 后台线程编码，
     * 这里只排队等待 native texture handle，避免 bridge 与响应式级联撞在同一帧。
     */
    const key = "qr-login";
    setQrTexKey("");
    scheduleQrTexture(
      key,
      url,
      () => {
        if (props.snapshot().url !== url) return;
        setQrTexKey(key);
        logMsg(`qr: 贴图已上传 ${key}`);
      },
      () => {
        if (props.snapshot().url !== url) return;
        /* 页面照常显示文字提示 */
        logMsg("qr: 贴图上传失败");
        setQrTexKey("");
      },
    );
  });

  onCleanup(() => cancelQrTexture("qr-login", lastQrUrl));

  const on = (i: number) => props.active() && props.cursor() === i;

  return (
    <View class="flex-col w-full grow gap-1 overflow-hidden">
      <View class="flex-row w-full items-center justify-center">
        {props.snapshot().url && qrTexKey() ? (
          <View class={bgCls("qrBox")}>
            <Image src={qrTexKey()} class="w-[124] h-[124]" />
            {expired() ? (
              <View class={bgCls("veil")} style={{ opacity: 0.6 }} />
            ) : null}
            {expired() ? (
              <View class="absolute inset-0 items-center justify-center">
                <View class={bgCls("badge")}>
                  <Text class={pTxt("badge")}>已过期</Text>
                </View>
              </View>
            ) : null}
          </View>
        ) : (
          <View class={bgCls("qrEmpty")}>
            <Text class={pTxt("empty")}>点刷新获取二维码</Text>
          </View>
        )}
      </View>

      <View class="flex-row w-full items-center justify-center">
        <Text class={pTxt("qrCaption")}>{caption()}</Text>
      </View>

      <View class="flex-row w-full items-center justify-center">
        <View class={on(0) ? bgCls("pillFocus") : bgCls("pill")}>
          <Text class={on(0) ? pTxt("accent") : pTxt("badge")}>刷新二维码</Text>
        </View>
      </View>

      <View class="flex-row w-full items-center justify-center">
        <MenuRow
          title="记住登录"
          value={props.remember() ? "ON" : "OFF"}
          focused={on(1)}
        />
      </View>
    </View>
  );
}
