;; Built-in macro 02: set the tempo from a Q8.8 BPM value carried in the two
;; argument bytes (integer part, fraction). Emits a standard Set Tempo
;; meta-event, which the engine also uses to update its tempo map.
(fn run [arg1 arg2]
  (let [bpm (+ (* arg1 256) arg2)]
    (when (> bpm 0)
      (let [micros (// (* 60000000 256) bpm)]
        (emit-meta 0x51 [(band (rshift micros 16) 0xFF)
                         (band (rshift micros 8) 0xFF)
                         (band micros 0xFF)])))))
