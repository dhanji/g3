package com.example

class Person(val name: String, val age: Int) {
    fun greet() {
        println("Hello, I'm $name")
    }
    
    fun getAge(): Int {
        return age
    }
}

interface Greeter {
    fun sayHello()
}

fun main() {
    val person = Person("Alice", 30)
    person.greet()
}

fun add(a: Int, b: Int): Int {
    return a + b
}
