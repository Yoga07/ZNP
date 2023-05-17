use std::cell::{Ref, RefCell};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

// Node of the linked list
#[derive(Clone, Debug)]
struct Node<T> {
    data: T,
    next: Option<Rc<RefCell<Node<T>>>>,
}

// Indexed Linked List
#[derive(Clone, Debug)]
pub struct IndexedList<T> {
    head: Option<Rc<RefCell<Node<T>>>>,
    tail: Option<Rc<RefCell<Node<T>>>>,
    length: usize,
}

impl<T> Iterator for IndexedList<T> {
    type Item = Rc<RefCell<Node<T>>>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(curr) = &self.head {
            curr.into_inner().next.clone()
        } else {
            None
        }
    }
}

impl<T> Deref for Node<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T> DerefMut for Node<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}


impl<T> IndexedList<T> {
    pub fn new() -> Self {
        IndexedList {
            head: None,
            tail: None,
            length: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub fn insert_at(&mut self, index: usize, data: T) {
        if index > self.length {
            self.push(data);
            return;
        }

        let new_node = Rc::new(RefCell::new(Node {
            data,
            next: None,
        }));

        if self.is_empty() {
            self.head = Some(new_node.clone());
            self.tail = Some(new_node.clone());
        } else if index == 0 {
            new_node.into_inner().next = self.head.take();
            self.head = Some(new_node);
        } else if index == self.length {
            self.tail.as_mut().unwrap().into_inner().next = Some(new_node.clone());
            self.tail = Some(new_node);
        } else {
            let mut current = self.head.clone();
            for _ in 0..index - 1 {
                current = current.unwrap().into_inner().next.clone();
            }
            let next_node = current.as_ref().unwrap().into_inner().next.clone();
            current.as_ref().unwrap().into_inner().next = Some(new_node.clone());
            new_node.into_inner().next = next_node;
        }

        self.length += 1;
    }

    fn push(&mut self, data: T) {
        let new_node = Rc::new(RefCell::new(Node {
            data,
            next: None,
        }));

        if self.is_empty() {
            self.head = Some(new_node.clone());
            self.tail = Some(new_node);
        } else {
            self.tail.as_mut().unwrap().into_inner().next = Some(new_node.clone());
            self.tail = Some(new_node);
        }

        self.length += 1;
    }
}