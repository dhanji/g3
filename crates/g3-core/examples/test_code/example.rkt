#lang racket

(define (greet name)
  (printf "Hello, ~a!\n" name))

(define (add x y)
  (+ x y))

(define (factorial n)
  (if (<= n 1)
      1
      (* n (factorial (- n 1)))))

(struct person (name age) #:transparent)

(define (person-greet p)
  (printf "Hello, I'm ~a\n" (person-name p)))

(greet "World")
(displayln (add 5 3))
(displayln (factorial 5))

(define alice (person "Alice" 30))
(person-greet alice)
