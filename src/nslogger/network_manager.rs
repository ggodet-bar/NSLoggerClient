use std::io ;
use std::sync::mpsc ;
use tokio_core::reactor::{Core,Timeout,Handle} ;
use futures::future::Either ;
use async_dnssd ;
use async_dnssd::Interface ;
use std::net::ToSocketAddrs ;
use std::time::Duration ;
use futures::{Stream,Future} ;

use nslogger::DEBUG_LOGGER ;
use nslogger::logger_state::HandlerMessageType ;

pub enum NetworkActionMessage {
    SetupBonjour(String),
}

enum BonjourServiceStatus {
    ServiceFound(String, String, u16),
    TimedOut,
    Unresolved,
}

pub struct NetworkManager {
    action_receiver:mpsc::Receiver<NetworkActionMessage>,
    message_sender:mpsc::Sender<HandlerMessageType>,

    core:Core,
    handle:Handle,
}

impl NetworkManager {
    pub fn new(action_receiver:mpsc::Receiver<NetworkActionMessage>,
               message_sender:mpsc::Sender<HandlerMessageType>) -> NetworkManager {
        let core = Core::new().unwrap() ;
        let handle = core.handle() ;
        NetworkManager {
            action_receiver:action_receiver,
            message_sender:message_sender,

            core:core,
            handle:handle
        }
    }
    pub fn run(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "starting network manager") ;
        }

        loop {
            match self.action_receiver.recv() {
                Ok(message) => {
                    if DEBUG_LOGGER {
                        info!(target:"NSLogger", "network manager received message") ;
                    }

                    match message {
                        NetworkActionMessage::SetupBonjour(service_name) => {
                            let mut is_connected = false ;
                            let mut current_delay:Option<u64> = None ;
                            while !is_connected {
                                match self.setup_bonjour(&service_name, current_delay) {
                                    Ok(BonjourServiceStatus::ServiceFound(bonjour_service_name, host, port)) => {
                                        self.message_sender.send(HandlerMessageType::TryConnectBonjour(bonjour_service_name, host, port)) ;
                                        is_connected = true ;
                                    }
                                    Ok(_) => {
                                        if DEBUG_LOGGER {
                                            info!(target:"NSLogger", "couldn't resolve Bonjour. Will retry in a few seconds") ;
                                        }

                                        current_delay = Some(10000) ;
                                    },
                                    Err(e) => {

                                    }
                                }
                            }
                        },
                        _ => ()
                    }

                },
                Err(e) => {
                    if DEBUG_LOGGER {
                        info!(target:"NSLogger", "network manager error: {:?}", e) ;
                    }

                    break ;
                }

            }
        }

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "stopping network manager") ;
        }
    }

    fn setup_bonjour(&mut self, service_name:&str, delay_ms:Option<u64>) -> io::Result<BonjourServiceStatus> {

        let listener = async_dnssd::browse(Interface::Any, service_name, None, &self.handle).unwrap() ;

        let delay_future = Timeout::new(Duration::from_millis(if delay_ms.is_some() { delay_ms.unwrap() } else { 0 }), &self.handle) ;
        let timeout = Timeout::new(Duration::from_secs(5), &self.handle).unwrap() ;
        match self.core.run(delay_future.and_then(|_| { Ok(listener.into_future()) }).unwrap().select2(timeout)) {
            Ok( either ) => {
                match either {
                   Either::A(( ( result, _ ), _ )) => {
                       let browse_result = result.unwrap() ;
                       if DEBUG_LOGGER {
                            info!(target:"NSLogger", "Browse result: {:?}", browse_result) ;
                            info!(target:"NSLogger", "Service name: {}", browse_result.service_name) ;
                       }
                        let bonjour_service_name = browse_result.service_name.to_string() ;
                        let mut remote_host: Option<String> = None ;
                        let mut remote_port: Option<u16> = None ;
                        match self.core.run(browse_result.resolve(&self.handle).unwrap().into_future()) {
                            Ok( (resolve_result, _) ) => {
                                let resolve_details = resolve_result.unwrap() ;
                                if DEBUG_LOGGER {
                                    info!(target:"NSLogger", "Service resolution details: {:?}", resolve_details) ;
                                }
                                for host_addr in format!("{}:{}", resolve_details.host_target, resolve_details.port).to_socket_addrs().unwrap() {


                                    if !host_addr.ip().is_global() && host_addr.ip().is_ipv4() {
                                        let ip_address = format!("{}", host_addr.ip()) ;
                                        if DEBUG_LOGGER {
                                            info!(target:"NSLogger", "Bonjour host details {:?}", host_addr) ;
                                        }
                                        remote_host = Some(ip_address) ;
                                        remote_port = Some(resolve_details.port) ;
                                        break ;
                                    }

                                }

                                return Ok(BonjourServiceStatus::ServiceFound(bonjour_service_name, remote_host.unwrap(), remote_port.unwrap())) ;
                            },
                            Err(_) => {
                                if DEBUG_LOGGER {
                                    warn!(target:"NSLogger", "Couldn't resolve Bonjour service")
                                }
                            }
                        } ;
                    },
                    Either::B( ( _, _ ) ) => {
                        if DEBUG_LOGGER {
                            warn!(target:"NSLogger", "Bonjour discovery timed out")
                        }

                        return Ok(BonjourServiceStatus::TimedOut) ;

                    }
                }
            },
            Err(_) => if DEBUG_LOGGER {
                warn!(target:"NSLogger", "Couldn't resolve Bonjour service")
            }

        } ;

        Ok(BonjourServiceStatus::Unresolved)
    }
}
