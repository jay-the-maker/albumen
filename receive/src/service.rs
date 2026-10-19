// SPDX-License-Identifier: Apache-2.0

//! Opt-in service definitions for the camera receiver.
//!
//! The generated file names the binary, the library, and the listen address.
//! It tells the supervisor to pass `ALBUMEN_RECEIVE_PASSWORD` through from the
//! user environment. The password is not written into the file. Nothing is
//! started.

use std::net::SocketAddr;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceHost {
    Linux,
    Macos,
    Windows,
}

impl ServiceHost {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Self::Linux => "albumen-receive.service",
            Self::Macos => "io.albumen.receive.plist",
            Self::Windows => "albumen-receive.xml",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceSpec<'a> {
    pub binary: &'a Path,
    pub library: &'a Path,
    pub user: &'a str,
    pub webdav: Option<SocketAddr>,
    pub sftp: Option<SocketAddr>,
    pub ftp: Option<SocketAddr>,
    pub member: Option<&'a str>,
}

pub fn render(host: ServiceHost, spec: &ServiceSpec<'_>) -> Result<String, String> {
    if spec.user.is_empty() {
        return Err("user must not be empty".to_string());
    }
    if spec.webdav.is_none() && spec.sftp.is_none() && spec.ftp.is_none() {
        return Err("pass at least one of --webdav, --sftp, or --ftp".to_string());
    }
    let args = arguments(spec);
    let text = match host {
        ServiceHost::Linux => systemd(&args),
        ServiceHost::Macos => launchd(&args),
        ServiceHost::Windows => task_xml(&args),
    };
    Ok(text)
}

fn arguments(spec: &ServiceSpec<'_>) -> Vec<String> {
    let mut args = vec![
        spec.binary.display().to_string(),
        "--to".to_string(),
        spec.library.display().to_string(),
        "--user".to_string(),
        spec.user.to_string(),
    ];
    if let Some(addr) = spec.webdav {
        args.push("--webdav".to_string());
        args.push(addr.to_string());
    }
    if let Some(addr) = spec.sftp {
        args.push("--sftp".to_string());
        args.push(addr.to_string());
    }
    if let Some(addr) = spec.ftp {
        args.push("--ftp".to_string());
        args.push(addr.to_string());
    }
    if let Some(member) = spec.member {
        args.push("--member".to_string());
        args.push(member.to_string());
    }
    args.push("--apply".to_string());
    args
}

fn systemd(args: &[String]) -> String {
    let exec = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "\
[Unit]
Description=Albumen camera receiver
Documentation=https://github.com/jay-the-maker/albumen/blob/main/docs/platforms.md

[Service]
Type=simple
Environment=ALBUMEN_RECEIVE_PASSWORD
ExecStart={exec}

[Install]
WantedBy=default.target
"
    )
}

fn launchd(args: &[String]) -> String {
    let mut program = String::new();
    for arg in args {
        program.push_str("    <string>");
        program.push_str(&xml_escape(arg));
        program.push_str("</string>\n");
    }
    format!(
        "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>io.albumen.receive</string>
  <key>ProgramArguments</key>
  <array>
{program}  </array>
  <!-- Set ALBUMEN_RECEIVE_PASSWORD in the environment before bootstrap. This file does not set it. -->
  <key>RunAtLoad</key>
  <false/>
</dict>
</plist>
"
    )
}

fn task_xml(args: &[String]) -> String {
    let command = xml_escape(&args[0]);
    let rest = args[1..]
        .iter()
        .map(|arg| xml_escape(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "\
<?xml version=\"1.0\" encoding=\"UTF-16\"?>
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">
  <RegistrationInfo>
    <Description>Albumen camera receiver. Set ALBUMEN_RECEIVE_PASSWORD in the user environment before enabling this task. This file does not contain a password.</Description>
  </RegistrationInfo>
  <Principals>
    <Principal>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <AllowHardTerminate>true</AllowHardTerminate>
    <Enabled>false</Enabled>
  </Settings>
  <Actions>
    <Exec>
      <Command>{command}</Command>
      <Arguments>{rest}</Arguments>
    </Exec>
  </Actions>
</Task>
"
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "/._-:@".contains(ch))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn spec() -> ServiceSpec<'static> {
        ServiceSpec {
            binary: Path::new("/opt/albumen/albumen-receive"),
            library: Path::new("/photos/library"),
            user: "camera",
            webdav: Some("127.0.0.1:8080".parse().unwrap()),
            sftp: None,
            ftp: None,
            member: None,
        }
    }

    #[test]
    fn each_host_names_the_listener_and_leaves_the_password_out() {
        for host in [ServiceHost::Linux, ServiceHost::Macos, ServiceHost::Windows] {
            let text = render(host, &spec()).unwrap();
            assert!(text.contains("--apply"), "{host:?}\n{text}");
            assert!(text.contains("--webdav"), "{host:?}\n{text}");
            assert!(text.contains("127.0.0.1:8080"), "{host:?}\n{text}");
            assert!(
                text.contains("ALBUMEN_RECEIVE_PASSWORD"),
                "{host:?}\n{text}"
            );
            assert!(text.contains("camera"), "{host:?}\n{text}");
            assert!(!text.contains("s3cret"), "{host:?}\n{text}");
            assert!(!text.contains("--password"), "{host:?}\n{text}");
        }
    }

    #[test]
    fn macos_does_not_start_at_load_and_windows_stays_disabled() {
        let text = render(ServiceHost::Macos, &spec()).unwrap();
        assert!(text.contains("<key>RunAtLoad</key>\n  <false/>"), "{text}");
        let text = render(ServiceHost::Windows, &spec()).unwrap();
        assert!(text.contains("<Enabled>false</Enabled>"), "{text}");
    }
}
