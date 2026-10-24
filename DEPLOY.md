# 对分易自动签到 · 服务器部署与使用

把对分易自动签到跑成**纯命令行常驻服务**,挂在这台服务器上。无窗口、无 GUI。

## 已经部署好的东西

| 项目 | 位置 |
|---|---|
| 服务 | `duifene-sign.service`(systemd,开机自启,`Restart=always`) |
| 守护进程二进制 | `/usr/local/bin/duifene-dfe daemon` |
| 统一管理命令 | `dfe`(root 下直接执行,自动以服务用户身份、同一配置运行) |
| 配置目录 | `/var/lib/duifene/config.json`(权限 600,含会话与授权码) |
| daemon 参数 | `/etc/duifene/daemon.env`(改后 `systemctl restart duifene-sign`) |
| 日志 | `journalctl -u duifene-sign -f` |

服务当前行为:
- **默认监控全部课程**,每轮**随机 100~140 秒**慢速探测(保活、防固定节律);
- 对配置了「高强度窗口」的课程,在窗口内自动切到**每 2 秒**高频探测,保证抢到那种只开放一两分钟的签到;窗口结束自动回落保活;
- daemon **每轮自动重读配置**,所以窗口/课程改完即时生效,**无需重启**;
- 会话掉线时服务不退出:进入"等待重新登录",你重新授权后自动恢复并重建监控;
- 可配 SMTP 邮件通知:签到成功、签到失败、登录态掉线都会发信。

## 一、首次授权登录(需要你在手机微信上操作一次)

登录态用**微信 OAuth 换 cookie** 的方式获得,二维码签到也要求微信登录态。

```bash
# 1) 打印微信授权链接
dfe login

# 2) 用手机/电脑微信打开上面的链接,点授权并跳转
# 3) 复制跳转后浏览器地址栏里"完整链接"(它带有 code= 参数)
# 4) 把链接粘回来执行:
dfe login 'https://www.duifene.com/...?...&code=xxxx...'
```

看到 `登录成功,会话已保存` 即完成。服务每 30 秒检查一次配置文件,检测到会话后会自动开始监控。

> 提示:微信 OAuth 的 code 是**一次性**的,授权链接请勿反复共用;cookie 会长期保存,服务端滑动续期也会自动写回配置。

## 二、查看课程,确认要监控/高频的课

```bash
dfe courses
```

输出示例:`1. 软件工程实践 (课堂ID: c12345)`。课程名支持子串匹配,课堂ID 需完全一致。

## 三、配置高强度轮询窗口(核心)

语法(可重复 `--weekday`,缺省=每天):

```bash
dfe schedule add --weekday 周五 --course 软件工程 --start 13:55 --end 17:15
dfe schedule add --weekday fri --course 高数 --start 10:00 --end 11:40 --interval 5 --warmup 10
dfe schedule list
dfe schedule clear
```

参数说明:

| 参数 | 含义 |
|---|---|
| `--course` | 匹配课程:课程名子串 或 课堂ID/课程ID 完全相等 |
| `--weekday` | `1-7`、`周一..周日`、`mon..sun`;可重复;缺省每天 |
| `--start / --end` | `HH:MM`;**含 end 分钟**(避免最后一分钟漏签) |
| `--interval` | 窗口内轮询间隔秒数,默认 `2`(便于测试也可设更大) |
| `--warmup` | 提前 N 分钟进入高频(例如老师提前发起签到) |

窗口内的课才会高频;没配窗口的课一直是慢速保活。已按你周五 13:55~17:15 软件工程 2 秒的窗口配置好。

## 四、(可选)邮件通知

需要你的发信邮箱(默认 QQ)开启 SMTP 并取得**授权码**(不是登录密码):
QQ 邮箱网页版 → 设置 → 账号 → 开启 POP3/SMTP → 生成授权码。

```bash
dfe mail set 你的QQ号@qq.com 授权码     # 收件人默认发给自己
dfe mail test                           # 发一封测试邮件验证
dfe mail clear                          # 清除
```

三种事件会发信:✅ 签到成功 / ⚠️ 签到失败(同一次活动只提醒一次,内部仍自动重试)/ 🔴 登录态掉线(附重新授权指引)。

## 五、日常运维

```bash
systemctl status duifene-sign          # 服务状态
journalctl -u duifene-sign -f          # 实时日志
systemctl restart duifene-sign         # 重启(改 daemon.env 后)

# 只监控指定课程(改 /etc/duifene/daemon.env,再重启服务):
#   DUIFENE_DAEMON_ARGS="--course 软件工程 --course 高数"
```

重新登录(掉线后):重复「一」的 `dfe login`,服务检测到新会话后自动恢复并发邮件告知。

## 六、重新编译升级(仅当改代码后)

```bash
cd /root/projects/duifene-auto-rs
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p duifene-core                       # 先跑测试
cargo build --release -p duifene-core
systemctl stop duifene-sign
cp target/release/dfe /usr/local/bin/duifene-dfe
systemctl start duifene-sign
```

## 注意事项

- **二维码签到必须微信登录态**,本方案登录流程即微信 OAuth,满足要求;
- 2 秒高频仅在你配置的窗口内对指定课程生效,其余时间回到 100~140s 随机,兼顾时效与风控;
- 定位签到建议先用 `dfe coords set <课程名> <经度> <纬度>` 给出教室坐标,否则会走自动三角定位;
- 服务以独立低权限用户 `duifene` 运行,配置文件仅该用户可读(600)。
