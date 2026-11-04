#!/bin/bash
# Installs the Cassandra macOS runner for this repository (docs/ci-runners.md). Run as root:
#   REG_TOKEN=$(gh api -X POST repos/bekirdag/pigeonpost/actions/runners/registration-token --jq .token) \
#     RUNNER_VERSION=2.338.2 sudo -E bash deploy/ci/cassandra-macos-runner.sh
# The token is single-use and short-lived; never store it.
# Creates the standard user pigeonpost-ci, denies it the owner's home, installs one GitHub runner
# for bekirdag/pigeonpost and runs it as a LaunchDaemon (UserName=pigeonpost-ci).
set -euo pipefail
CI_USER=pigeonpost-ci CI_UID=601 OWNER_HOME=/Users/bekirdag
NAME=${RUNNER_NAME:-cassandra-pigeonpost-1}
RUNNER_VERSION=${RUNNER_VERSION:?set RUNNER_VERSION}
: "${REG_TOKEN:?set REG_TOKEN}"
if ! id "$CI_USER" >/dev/null 2>&1; then
  pw=$(openssl rand -base64 33)   # never stored; the account is not meant for interactive login
  sysadminctl -addUser "$CI_USER" -fullName "Pigeonpost CI" -UID "$CI_UID" -password "$pw" -home "/Users/$CI_USER" -shell /bin/zsh
  unset pw
  createhomedir -c -u "$CI_USER" >/dev/null
  dscl . -create "/Users/$CI_USER" IsHidden 1
fi
chmod 700 "/Users/$CI_USER"
# Explicit deny wins over the staff-group r-x on the owner's home.
ls -led "$OWNER_HOME" | grep -q "user:$CI_USER deny" ||
  chmod +a "user:$CI_USER deny list,search,readattr,readextattr,readsecurity,read" "$OWNER_HOME"
dir="/Users/$CI_USER/actions-runner/$NAME"
sudo -u "$CI_USER" -H mkdir -p "$dir"
cd "$dir"
if [ ! -x ./config.sh ]; then
  sudo -u "$CI_USER" -H curl -fsSL -o runner.tgz \
    "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-osx-arm64-${RUNNER_VERSION}.tar.gz"
  sudo -u "$CI_USER" -H tar xzf runner.tgz && rm runner.tgz
fi
sudo -u "$CI_USER" -H ./config.sh --unattended --replace --url https://github.com/bekirdag/pigeonpost \
  --token "$REG_TOKEN" --name "$NAME" --labels cassandra-pigeonpost --work _work
unset REG_TOKEN
# The tarball ships the service wrapper in bin/; `svc.sh install` normally copies it next to
# config.sh. This script writes its own LaunchDaemon, so copy it here.
sudo -u "$CI_USER" -H cp bin/runsvc.sh ./runsvc.sh
sudo -u "$CI_USER" -H chmod 755 ./runsvc.sh
label="dev.pigeonpost.ci-runner.$NAME"
cat > "/Library/LaunchDaemons/$label.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$label</string>
  <key>UserName</key><string>$CI_USER</string>
  <key>GroupName</key><string>staff</string>
  <key>SessionCreate</key><true/>
  <key>ProgramArguments</key><array><string>$dir/runsvc.sh</string></array>
  <key>WorkingDirectory</key><string>$dir</string>
  <key>EnvironmentVariables</key><dict>
    <key>HOME</key><string>/Users/$CI_USER</string>
    <key>PATH</key><string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>ACTIONS_RUNNER_SVC</key><string>1</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>/Users/$CI_USER/Library/Logs/$label.log</string>
  <key>StandardErrorPath</key><string>/Users/$CI_USER/Library/Logs/$label.log</string>
</dict></plist>
PLIST
sudo -u "$CI_USER" -H mkdir -p "/Users/$CI_USER/Library/Logs"
chown root:wheel "/Library/LaunchDaemons/$label.plist"; chmod 644 "/Library/LaunchDaemons/$label.plist"
launchctl bootout "system/$label" 2>/dev/null || true
launchctl bootstrap system "/Library/LaunchDaemons/$label.plist"
echo "installed $label"
