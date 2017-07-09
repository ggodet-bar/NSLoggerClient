use std::sync::mpsc ;
use std::sync::{Arc,Mutex} ;


use nslogger::logger_state::{ HandlerMessageType, LoggerState } ;
use nslogger::message_handler::MessageHandler ;

use nslogger::DEBUG_LOGGER ;
use nslogger::{USE_SSL, BROWSE_BONJOUR} ;

pub struct MessageWorker
{
    pub shared_state:Arc<Mutex<LoggerState>>,
    pub message_sender:mpsc::Sender<HandlerMessageType>,
    handler:MessageHandler,
}


impl MessageWorker {

    pub fn new(logger_state:Arc<Mutex<LoggerState>>, message_sender:mpsc::Sender<HandlerMessageType>, handler_receiver:mpsc::Receiver<HandlerMessageType>) -> MessageWorker {
        let state_clone = logger_state.clone() ;
        MessageWorker{ shared_state: logger_state,
                       message_sender: message_sender,
                       handler: MessageHandler::new(handler_receiver, state_clone) }
    }

    pub fn run(&mut self) {
        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Logging thread starting up") ;
        }

        // Since we don't have a straightforward way to block the loop (cf Android), we'll setup
        // the connection before releasing the waiting thread(s).

        // Initial setup according to current parameters
        if self.shared_state.lock().unwrap().log_file_path.is_some() {
            self.shared_state.lock().unwrap().create_buffer_write_stream() ;
        }
        else if { let shared_state = self.shared_state.lock().unwrap() ;
                  shared_state.remote_host.is_some()
                    && shared_state.remote_port.is_some() } {
            self.shared_state.lock().unwrap().connect_to_remote() ;
        }
        else if !(self.shared_state.lock().unwrap().options & BROWSE_BONJOUR).is_empty() {
            self.shared_state.lock().unwrap().setup_bonjour() ;
        }


        // We are ready to run. Unpark the waiting threads now
        // (there may be multiple thread trying to start logging at the same time)
        self.shared_state.lock().unwrap().ready = true ;
        while !self.shared_state.lock().unwrap().ready_waiters.is_empty() {
            self.shared_state.lock().unwrap().ready_waiters.pop().unwrap().unpark() ;
        }

        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Starting log event loop") ;
        }

        // Process messages
        self.handler.run_loop() ;


        if DEBUG_LOGGER {
            info!(target:"NSLogger", "Logging thread looper ended") ;
        }

        // Once loop exists, reset the variable (in case of problem we'll recreate a thread)
        self.shared_state.lock().unwrap().close_bonjour() ;
        self.shared_state.lock().unwrap().close_buffer_write_stream() ;
        //loggingThread = null;
        //loggingThreadHandler = null;
    }
}
